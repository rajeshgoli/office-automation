#!/usr/bin/env python3
"""
macOS Occupancy Detector

Sends raw occupancy data to orchestrator:
1. Timestamp of last keyboard/mouse activity
2. External monitor connection status

The orchestrator state machine decides what "active" means based on
timestamp comparisons with door events - no policy decisions here!

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
from pathlib import Path
from oauth_device_client import OAuthDeviceClient


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
    oauth_client: Optional[OAuthDeviceClient] = None,
    # Legacy Basic Auth (deprecated)
    auth_username: Optional[str] = None,
    auth_password: Optional[str] = None
) -> bool:
    """
    Send occupancy state to the orchestrator via HTTP POST.

    Args:
        state: Current occupancy state
        orchestrator_url: Base URL of the orchestrator (e.g., http://localhost:8080)
        auth_username: Optional HTTP Basic Auth username
        auth_password: Optional HTTP Basic Auth password

    Returns:
        True if successful, False otherwise
    """
    url = f"{orchestrator_url.rstrip('/')}/occupancy"
    # Send timestamp of last activity, not a boolean
    # Let the orchestrator/state machine decide what "active" means
    last_active_timestamp = time.time() - state.idle_seconds
    payload = json.dumps({
        "last_active_timestamp": last_active_timestamp,
        "external_monitor": state.external_monitor_connected
    }).encode("utf-8")

    headers = {"Content-Type": "application/json"}

    try:
        # OAuth authentication (preferred)
        if oauth_client:
            token = oauth_client.get_access_token()
            if not token:
                print("Failed to get access token", file=sys.stderr)
                return False
            headers["Authorization"] = f"Bearer {token}"

        # Legacy Basic Auth (deprecated)
        elif auth_username and auth_password:
            password_mgr = urllib.request.HTTPPasswordMgrWithDefaultRealm()
            password_mgr.add_password(None, orchestrator_url, auth_username, auth_password)
            auth_handler = urllib.request.HTTPBasicAuthHandler(password_mgr)
            opener = urllib.request.build_opener(auth_handler)
            urllib.request.install_opener(opener)

        req = urllib.request.Request(url, data=payload, headers=headers, method="POST")

        with urllib.request.urlopen(req, timeout=5) as response:
            result = json.loads(response.read().decode("utf-8"))
            return result.get("ok", False)

    except urllib.error.HTTPError as e:
        if e.code == 401:
            print("Authentication failed - token may be expired", file=sys.stderr)
            if oauth_client:
                print("Run with --reauth to re-authenticate", file=sys.stderr)
        else:
            print(f"HTTP error {e.code}: {e.reason}", file=sys.stderr)
        return False
    except urllib.error.URLError as e:
        print(f"Error sending to orchestrator: {e}", file=sys.stderr)
        return False
    except Exception as e:
        print(f"Unexpected error: {e}", file=sys.stderr)
        return False


def check_occupancy(idle_threshold_seconds: float = 30) -> OccupancyState:
    """
    Check current occupancy state.

    Args:
        idle_threshold_seconds: Seconds of idle time before considered inactive (default 30s)

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
    idle_threshold: float = 30,
    on_change: Optional[Callable[[OccupancyState, OccupancyState], None]] = None,
    output_json: bool = False,
    orchestrator_url: Optional[str] = None,
    heartbeat_interval: float = 60.0,
    oauth_client: Optional[OAuthDeviceClient] = None,
    auth_username: Optional[str] = None,
    auth_password: Optional[str] = None
):
    """
    Continuously monitor occupancy state.

    Args:
        poll_interval: Seconds between checks
        idle_threshold: Seconds before considered idle (for local console display only)
        on_change: Callback when state changes (old_state, new_state)
        output_json: Output state as JSON on each change
        orchestrator_url: URL to POST state changes to (sends raw timestamp, not boolean)
        heartbeat_interval: Seconds between heartbeat sends (even if no change)
    """
    last_state: Optional[OccupancyState] = None
    last_send_time: float = 0

    print(f"Watching occupancy (poll: {poll_interval}s, idle threshold: {idle_threshold}s)")
    if orchestrator_url:
        print(f"Sending state to: {orchestrator_url}/occupancy")
        print(f"Heartbeat every {heartbeat_interval}s")
    print("Press Ctrl+C to stop\n")

    try:
        while True:
            state = check_occupancy(idle_threshold)
            now = time.time()

            # Detect state change
            state_changed = (
                last_state is None or
                last_state.is_present != state.is_present
            )

            # Send on change OR heartbeat interval
            heartbeat_due = (now - last_send_time) >= heartbeat_interval
            should_send = state_changed or heartbeat_due

            if should_send:
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
                    if send_to_orchestrator(state, orchestrator_url, oauth_client, auth_username, auth_password):
                        last_send_time = now
                        reason = "change" if state_changed else "heartbeat"
                        print(f"  → Sent to orchestrator ({reason})")
                    else:
                        print(f"  → Failed to send to orchestrator", file=sys.stderr)

                if on_change and last_state is not None:
                    on_change(last_state, state)

                if state_changed:
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
    parser.add_argument("--idle-threshold", "-i", type=float, default=30,
                        help="Idle threshold in seconds (default: 30)")
    parser.add_argument("--url", "-u", type=str, default="http://localhost:8080",
                        help="Orchestrator URL (default: http://localhost:8080)")
    parser.add_argument("--no-send", action="store_true",
                        help="Don't send state changes to orchestrator")
    parser.add_argument("--auth-token-file", type=str,
                        help="OAuth token file path")
    parser.add_argument("--reauth", action="store_true",
                        help="Force re-authentication")
    parser.add_argument("--auth-username", type=str,
                        help="HTTP Basic Auth username (deprecated)")
    parser.add_argument("--auth-password", type=str,
                        help="HTTP Basic Auth password (deprecated)")

    args = parser.parse_args()

    orchestrator_url = None if args.no_send else args.url

    # Set up OAuth client
    oauth_client = None
    if orchestrator_url:
        token_file = Path(args.auth_token_file) if args.auth_token_file else None
        oauth_client = OAuthDeviceClient(orchestrator_url, token_file)

        # Load existing token or authenticate
        if args.reauth or not oauth_client.load_token():
            if not oauth_client.authenticate():
                print("Authentication failed", file=sys.stderr)
                sys.exit(1)

    if args.watch:
        watch_occupancy(
            poll_interval=args.poll,
            idle_threshold=args.idle_threshold,
            output_json=args.json,
            orchestrator_url=orchestrator_url,
            oauth_client=oauth_client,
            auth_username=args.auth_username,
            auth_password=args.auth_password
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
