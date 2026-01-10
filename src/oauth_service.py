"""
Google OAuth 2.0 service for office automation.

Implements:
- Authorization Code Flow with PKCE (for web dashboard)
- Device Flow (for headless occupancy detector)
- JWT token generation and validation
- Email allowlist enforcement
"""

import secrets
import hashlib
import base64
import json
import logging
from datetime import datetime, timedelta
from typing import Optional, Dict, Any
from dataclasses import dataclass

import jwt
from google.oauth2 import id_token
from google.auth.transport import requests as google_requests
from google_auth_oauthlib.flow import Flow

logger = logging.getLogger(__name__)


@dataclass
class UserSession:
    """User session data."""
    email: str
    access_token: str
    refresh_token: Optional[str]
    expires_at: datetime

    def is_expired(self) -> bool:
        return datetime.now() >= self.expires_at


class OAuthService:
    """Google OAuth 2.0 service."""

    def __init__(
        self,
        client_id: str,
        client_secret: str,
        allowed_emails: list[str],
        token_expiry_days: int = 7,
        redirect_uri: Optional[str] = None,
        jwt_secret: Optional[str] = None,
        trusted_networks: Optional[list[str]] = None
    ):
        self.client_id = client_id
        self.client_secret = client_secret
        self.allowed_emails = [e.lower() for e in allowed_emails]
        self.token_expiry_days = token_expiry_days
        self.redirect_uri = redirect_uri
        self.trusted_networks = trusted_networks or []

        # JWT secret for signing tokens
        self.jwt_secret = jwt_secret or secrets.token_urlsafe(32)

        # In-memory session storage (email -> UserSession)
        # For production with multiple instances, use Redis/database
        self._sessions: Dict[str, UserSession] = {}

        # Device flow state (device_code -> {user_code, verification_url, ...})
        self._device_flow_requests: Dict[str, Dict[str, Any]] = {}

    def generate_pkce_pair(self) -> tuple[str, str]:
        """Generate PKCE code_verifier and code_challenge."""
        code_verifier = secrets.token_urlsafe(32)
        code_challenge = base64.urlsafe_b64encode(
            hashlib.sha256(code_verifier.encode()).digest()
        ).decode().rstrip('=')
        return code_verifier, code_challenge

    def create_authorization_url(self, state: str, code_challenge: str) -> str:
        """Create Google OAuth authorization URL for web flow."""
        flow = Flow.from_client_config(
            {
                "web": {
                    "client_id": self.client_id,
                    "client_secret": self.client_secret,
                    "auth_uri": "https://accounts.google.com/o/oauth2/auth",
                    "token_uri": "https://oauth2.googleapis.com/token",
                }
            },
            scopes=[
                "openid",
                "https://www.googleapis.com/auth/userinfo.email",
                "https://www.googleapis.com/auth/userinfo.profile"
            ],
            redirect_uri=self.redirect_uri
        )

        authorization_url, _ = flow.authorization_url(
            access_type='offline',
            include_granted_scopes='true',
            state=state,
            code_challenge=code_challenge,
            code_challenge_method='S256',
            prompt='consent'  # Force consent to get refresh token
        )

        return authorization_url

    async def exchange_code_for_token(
        self,
        code: str,
        code_verifier: str,
        redirect_uri: Optional[str] = None
    ) -> Optional[UserSession]:
        """Exchange authorization code for tokens."""
        flow = Flow.from_client_config(
            {
                "web": {
                    "client_id": self.client_id,
                    "client_secret": self.client_secret,
                    "auth_uri": "https://accounts.google.com/o/oauth2/auth",
                    "token_uri": "https://oauth2.googleapis.com/token",
                }
            },
            scopes=[
                "openid",
                "https://www.googleapis.com/auth/userinfo.email",
                "https://www.googleapis.com/auth/userinfo.profile"
            ],
            redirect_uri=redirect_uri or self.redirect_uri
        )

        try:
            flow.fetch_token(code=code, code_verifier=code_verifier)

            # Verify ID token
            credentials = flow.credentials
            id_info = id_token.verify_oauth2_token(
                credentials.id_token,
                google_requests.Request(),
                self.client_id
            )

            email = id_info.get('email', '').lower()

            # Check email allowlist
            if email not in self.allowed_emails:
                logger.warning(f"Rejected login from non-allowed email: {email}")
                return None

            # Create session
            session = UserSession(
                email=email,
                access_token=credentials.token,
                refresh_token=credentials.refresh_token,
                expires_at=datetime.now() + timedelta(days=self.token_expiry_days)
            )

            self._sessions[email] = session
            logger.info(f"User logged in: {email}")

            return session

        except Exception as e:
            logger.error(f"Token exchange failed: {e}")
            return None

    def generate_jwt(self, email: str) -> str:
        """Generate JWT token for API authentication."""
        payload = {
            'email': email,
            'exp': datetime.utcnow() + timedelta(days=self.token_expiry_days),
            'iat': datetime.utcnow()
        }
        return jwt.encode(payload, self.jwt_secret, algorithm='HS256')

    def verify_jwt(self, token: str) -> Optional[str]:
        """Verify JWT token and return email if valid."""
        try:
            payload = jwt.decode(token, self.jwt_secret, algorithms=['HS256'])
            email = payload.get('email', '').lower()

            # Check email still allowed
            if email not in self.allowed_emails:
                logger.warning(f"Token rejected: email {email} no longer allowed")
                return None

            return email

        except jwt.ExpiredSignatureError:
            logger.debug("Token expired")
            return None
        except jwt.InvalidTokenError as e:
            logger.warning(f"Invalid token: {e}")
            return None

    def logout(self, email: str):
        """Logout user and invalidate session."""
        self._sessions.pop(email.lower(), None)
        logger.info(f"User logged out: {email}")

    # Device Flow Implementation (for occupancy detector)

    def initiate_device_flow(self) -> Dict[str, str]:
        """Initiate device authorization flow."""
        import requests

        response = requests.post(
            'https://oauth2.googleapis.com/device/code',
            data={
                'client_id': self.client_id,
                'scope': 'openid email profile'
            }
        )

        if response.status_code != 200:
            raise Exception(f"Device flow initiation failed: {response.text}")

        data = response.json()

        # Store device flow state
        self._device_flow_requests[data['device_code']] = {
            'user_code': data['user_code'],
            'verification_url': data['verification_url'],
            'expires_at': datetime.now() + timedelta(seconds=data['expires_in']),
            'interval': data.get('interval', 5)
        }

        return {
            'device_code': data['device_code'],
            'user_code': data['user_code'],
            'verification_url': data['verification_url'],
            'expires_in': data['expires_in']
        }

    def poll_device_flow(self, device_code: str) -> Optional[Dict[str, Any]]:
        """Poll device flow for authorization completion."""
        import requests

        if device_code not in self._device_flow_requests:
            return {'status': 'invalid', 'message': 'Unknown device code'}

        request_data = self._device_flow_requests[device_code]

        # Check expiration
        if datetime.now() >= request_data['expires_at']:
            del self._device_flow_requests[device_code]
            return {'status': 'expired', 'message': 'Device code expired'}

        # Poll Google
        response = requests.post(
            'https://oauth2.googleapis.com/token',
            data={
                'client_id': self.client_id,
                'client_secret': self.client_secret,
                'device_code': device_code,
                'grant_type': 'urn:ietf:params:oauth:grant-type:device_code'
            }
        )

        if response.status_code == 200:
            # Success!
            token_data = response.json()

            # Verify ID token
            try:
                id_info = id_token.verify_oauth2_token(
                    token_data['id_token'],
                    google_requests.Request(),
                    self.client_id
                )

                email = id_info.get('email', '').lower()

                # Check email allowlist
                if email not in self.allowed_emails:
                    logger.warning(f"Device flow: rejected non-allowed email {email}")
                    return {'status': 'forbidden', 'message': 'Email not allowed'}

                # Generate JWT for device
                jwt_token = self.generate_jwt(email)

                # Cleanup
                del self._device_flow_requests[device_code]

                return {
                    'status': 'success',
                    'email': email,
                    'access_token': jwt_token,
                    'refresh_token': token_data.get('refresh_token'),
                    'expires_in': timedelta(days=self.token_expiry_days).total_seconds()
                }

            except Exception as e:
                logger.error(f"Device flow token verification failed: {e}")
                return {'status': 'error', 'message': str(e)}

        elif response.status_code == 428:
            # Still pending
            return {'status': 'pending', 'message': 'User has not authorized yet'}

        elif response.status_code == 400:
            error = response.json().get('error')
            if error == 'authorization_pending':
                return {'status': 'pending', 'message': 'Waiting for user authorization'}
            elif error == 'slow_down':
                return {'status': 'slow_down', 'message': 'Polling too fast'}
            else:
                return {'status': 'error', 'message': error}

        else:
            return {'status': 'error', 'message': f'HTTP {response.status_code}'}
