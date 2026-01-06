#!/usr/bin/env python3
"""
macOS Occupancy Detector

Detects presence based on:
1. External monitor connected
2. Mac is active (not idle beyond threshold)

Usage:
    python occupancy_detector.py              # Single check
    python occupancy_detector.py --watch      # Continuous monitoring
    python occupancy_detector.py --json       # Output as JSON
"""

import subprocess
import time
import argparse
import json
import sys
import urllib.request
import urllib.error
from dataclasses import dataclass
from typing import Optional, Callable


@dataclass
class OccupancyState:
    is_present: bool
    external_monitor_connected: bool
    idle_seconds: float
    display_count: int
    external_displays: list[str]


def get_idle_time_seconds() -> float:
    """Get system idle time in seconds using IOKit."""
    try:
        result = subprocess.run(
            ["ioreg", "-c", "IOHIDSystem"],
            capture_output=True,
            text=True,
            timeout=5
        )
        for line in result.stdout.split("\n"):
            if "HIDIdleTime" in line:
                # Value is in nanoseconds
                parts = line.split()
                if parts:
                    nanoseconds = int(parts[-1])
                    return nanoseconds / 1_000_000_000
    except Exception as e:
        print(f"Error getting idle time: {e}", file=sys.stderr)
    return 0.0


def get_display_info() -> tuple[int, list[str]]:
    """
    Get display information.
    Returns (total_display_count, list_of_external_display_names)
    """
    external_displays = []
    all_displays = []

    try:
        result = subprocess.run(
            ["system_profiler", "SPDisplaysDataType"],
            capture_output=True,
            text=True,
            timeout=10
        )

        lines = result.stdout.split("\n")
        i = 0
        while i < len(lines):
            line = lines[i]
            stripped = line.strip()

            # Skip known non-display headers
            skip_prefixes = ("Chipset", "Type:", "Bus:", "Vendor:", "Metal", "Total",
                           "Graphics/Displays:", "Apple M", "Resolution", "Display Type",
                           "Main Display", "Mirror", "Online", "Rotation", "Connection",
                           "Automatically", "UI Looks")

            if stripped.endswith(":") and not any(stripped.startswith(p) for p in skip_prefixes):
                display_name = stripped.rstrip(":")

                # Look ahead to verify this is a display (has Resolution or Display Type)
                is_display = False
                is_internal = False
                for j in range(i + 1, min(i + 15, len(lines))):
                    check_line = lines[j]
                    if "Resolution" in check_line:
                        is_display = True
                    if "Built-in" in check_line or "Connection Type: Internal" in check_line:
                        is_internal = True
                    # Stop at next section
                    if check_line.strip().endswith(":") and ":" not in check_line.strip()[:-1]:
                        if not any(check_line.strip().startswith(p) for p in skip_prefixes):
                            break

                if is_display:
                    all_displays.append(display_name)
                    if not is_internal:
                        external_displays.append(display_name)

            i += 1

    except Exception as e:
        print(f"Error getting display info: {e}", file=sys.stderr)

    return len(all_displays), external_displays


def send_to_orchestrator(
    state: OccupancyState,
    orchestrator_url: str,
    idle_threshold: float = 300
) -> bool:
    """
    Send occupancy state to the orchestrator via HTTP POST.

    Args:
        state: Current occupancy state
        orchestrator_url: Base URL of the orchestrator (e.g., http://localhost:8080)
        idle_threshold: Seconds of idle time before considered inactive

    Returns:
        True if successful, False otherwise
    """
    url = f"{orchestrator_url.rstrip('/')}/occupancy"
    payload = json.dumps({
        "active": state.idle_seconds < idle_threshold,  # active if not idle
        "external_monitor": state.external_monitor_connected
    }).encode("utf-8")

    try:
        req = urllib.request.Request(
            url,
            data=payload,
            headers={"Content-Type": "application/json"},
            method="POST"
        )
        with urllib.request.urlopen(req, timeout=5) as response:
            result = json.loads(response.read().decode("utf-8"))
            return result.get("ok", False)
    except urllib.error.URLError as e:
        print(f"Error sending to orchestrator: {e}", file=sys.stderr)
        return False
    except Exception as e:
        print(f"Unexpected error: {e}", file=sys.stderr)
        return False


def check_occupancy(idle_threshold_seconds: float = 300) -> OccupancyState:
    """
    Check current occupancy state.

    Args:
        idle_threshold_seconds: Seconds of idle time before considered inactive (default 5 min)

    Returns:
        OccupancyState with all detection details
    """
    idle_seconds = get_idle_time_seconds()
    display_count, external_displays = get_display_info()

    external_monitor_connected = len(external_displays) > 0
    is_active = idle_seconds < idle_threshold_seconds

    is_present = external_monitor_connected and is_active

    return OccupancyState(
        is_present=is_present,
        external_monitor_connected=external_monitor_connected,
        idle_seconds=idle_seconds,
        display_count=display_count,
        external_displays=external_displays
    )


def watch_occupancy(
    poll_interval: float = 5.0,
    idle_threshold: float = 300,
    on_change: Optional[Callable[[OccupancyState, OccupancyState], None]] = None,
    output_json: bool = False,
    orchestrator_url: Optional[str] = None
):
    """
    Continuously monitor occupancy state.

    Args:
        poll_interval: Seconds between checks
        idle_threshold: Seconds before considered idle
        on_change: Callback when state changes (old_state, new_state)
        output_json: Output state as JSON on each change
        orchestrator_url: URL to POST state changes to (e.g., http://localhost:8080)
    """
    last_state: Optional[OccupancyState] = None

    print(f"Watching occupancy (poll: {poll_interval}s, idle threshold: {idle_threshold}s)")
    if orchestrator_url:
        print(f"Sending state to: {orchestrator_url}/occupancy")
    print("Press Ctrl+C to stop\n")

    try:
        while True:
            state = check_occupancy(idle_threshold)

            # Detect state change
            state_changed = (
                last_state is None or
                last_state.is_present != state.is_present
            )

            if state_changed:
                if output_json:
                    print(json.dumps({
                        "timestamp": time.time(),
                        "is_present": state.is_present,
                        "external_monitor": state.external_monitor_connected,
                        "idle_seconds": round(state.idle_seconds, 1),
                        "displays": state.external_displays
                    }))
                else:
                    status = "PRESENT" if state.is_present else "AWAY"
                    print(f"[{time.strftime('%H:%M:%S')}] {status} | "
                          f"monitors: {state.external_displays} | "
                          f"idle: {state.idle_seconds:.0f}s")

                # Send to orchestrator
                if orchestrator_url:
                    if send_to_orchestrator(state, orchestrator_url, idle_threshold):
                        print(f"  → Sent to orchestrator")
                    else:
                        print(f"  → Failed to send to orchestrator", file=sys.stderr)

                if on_change and last_state is not None:
                    on_change(last_state, state)

                last_state = state

            time.sleep(poll_interval)

    except KeyboardInterrupt:
        print("\nStopped watching.")


def main():
    parser = argparse.ArgumentParser(description="macOS Occupancy Detector")
    parser.add_argument("--watch", "-w", action="store_true",
                        help="Continuously monitor occupancy")
    parser.add_argument("--json", "-j", action="store_true",
                        help="Output as JSON")
    parser.add_argument("--poll", "-p", type=float, default=5.0,
                        help="Poll interval in seconds (default: 5)")
    parser.add_argument("--idle-threshold", "-i", type=float, default=300,
                        help="Idle threshold in seconds (default: 300)")
    parser.add_argument("--url", "-u", type=str, default="http://localhost:8080",
                        help="Orchestrator URL (default: http://localhost:8080)")
    parser.add_argument("--no-send", action="store_true",
                        help="Don't send state changes to orchestrator")

    args = parser.parse_args()

    orchestrator_url = None if args.no_send else args.url

    if args.watch:
        watch_occupancy(
            poll_interval=args.poll,
            idle_threshold=args.idle_threshold,
            output_json=args.json,
            orchestrator_url=orchestrator_url
        )
    else:
        state = check_occupancy(args.idle_threshold)

        if args.json:
            print(json.dumps({
                "is_present": state.is_present,
                "external_monitor_connected": state.external_monitor_connected,
                "idle_seconds": round(state.idle_seconds, 1),
                "display_count": state.display_count,
                "external_displays": state.external_displays
            }, indent=2))
        else:
            status = "PRESENT" if state.is_present else "AWAY"
            print(f"Status: {status}")
            print(f"External monitors: {state.external_displays or 'None'}")
            print(f"Idle time: {state.idle_seconds:.1f}s")

        sys.exit(0 if state.is_present else 1)


if __name__ == "__main__":
    main()
