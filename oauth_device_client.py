"""
OAuth Device Flow client for occupancy detector.

Implements Google OAuth 2.0 Device Flow for headless authentication.
"""

import json
import time
import sys
from pathlib import Path
from typing import Optional
import urllib.request
import urllib.error


class OAuthDeviceClient:
    """OAuth Device Flow client."""

    def __init__(self, orchestrator_url: str, token_file: Optional[Path] = None):
        self.orchestrator_url = orchestrator_url.rstrip('/')
        self.token_file = token_file or Path.home() / '.office-automate' / 'auth_token.json'
        self.token_file.parent.mkdir(parents=True, exist_ok=True)

        self._access_token: Optional[str] = None
        self._refresh_token: Optional[str] = None
        self._token_expiry: Optional[float] = None

    def load_token(self) -> bool:
        """Load token from file."""
        if not self.token_file.exists():
            return False

        try:
            with open(self.token_file, 'r') as f:
                data = json.load(f)

            self._access_token = data.get('access_token')
            self._refresh_token = data.get('refresh_token')
            self._token_expiry = data.get('expires_at')

            # Check if token expired
            if self._token_expiry and time.time() >= self._token_expiry:
                print("Token expired, need to re-authenticate", file=sys.stderr)
                return False

            return bool(self._access_token)

        except Exception as e:
            print(f"Failed to load token: {e}", file=sys.stderr)
            return False

    def save_token(self, access_token: str, refresh_token: Optional[str], expires_in: float):
        """Save token to file."""
        data = {
            'access_token': access_token,
            'refresh_token': refresh_token,
            'expires_at': time.time() + expires_in
        }

        with open(self.token_file, 'w') as f:
            json.dump(data, f, indent=2)

        # Restrict permissions
        self.token_file.chmod(0o600)

        self._access_token = access_token
        self._refresh_token = refresh_token
        self._token_expiry = data['expires_at']

    def authenticate(self) -> bool:
        """Perform device flow authentication."""
        print("\n=== Office Climate Occupancy Detector Authentication ===", file=sys.stderr)
        print("Starting device authorization flow...\n", file=sys.stderr)

        # Start device flow
        try:
            req = urllib.request.Request(
                f"{self.orchestrator_url}/auth/device/start",
                method='POST',
                headers={'Content-Type': 'application/json'}
            )

            with urllib.request.urlopen(req, timeout=10) as response:
                result = json.loads(response.read().decode())

            device_code = result['device_code']
            user_code = result['user_code']
            verification_url = result['verification_url']
            expires_in = result['expires_in']

            print(f"Please visit: {verification_url}", file=sys.stderr)
            print(f"And enter code: {user_code}\n", file=sys.stderr)
            print(f"Waiting for authorization (expires in {expires_in}s)...", file=sys.stderr)

            # Poll for completion
            poll_interval = 5
            start_time = time.time()

            while time.time() - start_time < expires_in:
                time.sleep(poll_interval)

                # Poll
                poll_data = json.dumps({'device_code': device_code}).encode()
                poll_req = urllib.request.Request(
                    f"{self.orchestrator_url}/auth/device/poll",
                    data=poll_data,
                    method='POST',
                    headers={'Content-Type': 'application/json'}
                )

                try:
                    with urllib.request.urlopen(poll_req, timeout=10) as response:
                        poll_result = json.loads(response.read().decode())

                    status = poll_result.get('status')

                    if status == 'success':
                        print("\n✓ Authorization successful!", file=sys.stderr)
                        print(f"Logged in as: {poll_result['email']}\n", file=sys.stderr)

                        # Save token
                        self.save_token(
                            poll_result['access_token'],
                            poll_result.get('refresh_token'),
                            poll_result['expires_in']
                        )

                        return True

                    elif status == 'pending':
                        print(".", end='', file=sys.stderr, flush=True)

                    elif status == 'slow_down':
                        poll_interval += 2

                    elif status == 'expired':
                        print("\n✗ Authorization expired", file=sys.stderr)
                        return False

                    elif status == 'forbidden':
                        print(f"\n✗ {poll_result.get('message', 'Email not authorized')}", file=sys.stderr)
                        return False

                    else:
                        print(f"\n✗ Error: {poll_result.get('message', 'Unknown error')}", file=sys.stderr)
                        return False

                except urllib.error.URLError as e:
                    print(f"\n✗ Poll failed: {e}", file=sys.stderr)
                    return False

            print("\n✗ Authorization timed out", file=sys.stderr)
            return False

        except Exception as e:
            print(f"✗ Authentication failed: {e}", file=sys.stderr)
            return False

    def get_access_token(self) -> Optional[str]:
        """Get current access token (refreshing if needed)."""
        # Check if token expired
        if self._token_expiry and time.time() >= self._token_expiry - 300:  # Refresh 5min early
            print("Token expiring soon, re-authenticating...", file=sys.stderr)
            if not self.authenticate():
                return None

        return self._access_token
