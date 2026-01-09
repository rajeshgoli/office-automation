# Google OAuth Implementation - Session Handoff

**Date**: 2026-01-09
**Commits**: a728dbd (backend) → 72dcc48 (frontend/detector) → 88645f6 (docs)
**Status**: ✅ COMPLETE - Ready for Testing

---

## What's Been Implemented ✅

### 1. Backend OAuth Service (Complete)

**Files Created:**
- `src/oauth_service.py` - Core OAuth logic
- `oauth_device_client.py` - Device flow client for occupancy detector

**Files Modified:**
- `requirements.txt` - Added OAuth dependencies
- `src/config.py` - Added GoogleOAuthConfig dataclass
- `src/orchestrator.py` - OAuth endpoints, JWT middleware, WebSocket auth
- `config.yaml` - OAuth configuration placeholders

### 2. OAuth Features Implemented

#### Authorization Code Flow (Web Dashboard)
- **PKCE Support**: Secure authorization without client secret exposure
- **JWT Token Generation**: 7-day tokens with email allowlist enforcement
- **Endpoints**:
  - `GET /auth/login` - Initiate OAuth flow, returns Google authorization URL
  - `GET /auth/callback` - Handle OAuth redirect, exchange code for JWT
  - `POST /auth/logout` - Invalidate session

#### Device Flow (Occupancy Detector)
- **Headless Authentication**: Shows device code, polls for approval
- **Endpoints**:
  - `POST /auth/device/start` - Initiate device flow
  - `POST /auth/device/poll` - Check authorization status
- **Client Implementation**: `OAuthDeviceClient` class with token storage

#### JWT Middleware
- **Bearer Token Validation**: Replaces HTTP Basic Auth
- **Email Allowlist**: Only rajeshgoli+kumo@gmail.com allowed
- **Fallback**: Basic Auth still works if OAuth not configured
- **Skip Paths**: OAuth endpoints and WebSocket bypass middleware

#### WebSocket Authentication
- **First Message Auth**: Client must send `{"type": "auth", "token": "JWT"}`
- **Auto-Close**: Invalid token → close with code 4001
- **Backwards Compatible**: No auth if OAuth not enabled

---

## Implementation Summary

### 3. Frontend Implementation (✅ Complete - Commit 72dcc48)

#### A. Token Management (`frontend/api.ts`)
**Add after line 15:**
```typescript
const TOKEN_KEY = 'auth_token';
const EMAIL_KEY = 'user_email';

export function getAuthToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setAuthToken(token: string, email: string) {
  localStorage.setItem(TOKEN_KEY, token);
  localStorage.setItem(EMAIL_KEY, email);
}

export function clearAuthToken() {
  localStorage.removeItem(TOKEN_KEY);
  localStorage.removeItem(EMAIL_KEY);
}

export function getUserEmail(): string | null {
  return localStorage.getItem(EMAIL_KEY);
}

export function isAuthenticated(): boolean {
  return getAuthToken() !== null;
}

export async function startLogin(): Promise<{ authorization_url: string }> {
  const response = await fetch(`${API_BASE}/auth/login`);
  if (!response.ok) {
    throw new Error('Failed to initiate login');
  }
  return response.json();
}

export async function logout(): Promise<void> {
  const token = getAuthToken();
  if (token) {
    await fetch(`${API_BASE}/auth/logout`, {
      method: 'POST',
      headers: { 'Authorization': `Bearer ${token}` }
    });
  }
  clearAuthToken();
}
```

#### B. Update All API Calls (`frontend/api.ts`)
**Pattern to apply to `fetchStatus`, `setERVSpeed`, `setHVACMode`:**
```typescript
export async function fetchStatus(): Promise<ApiStatus> {
  const token = getAuthToken();
  const headers: HeadersInit = {};

  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  const response = await fetch(`${API_BASE}/status`, { headers });

  if (response.status === 401) {
    clearAuthToken();
    window.location.reload();
    throw new Error('Authentication required');
  }

  return response.json();
}
```

#### C. Update WebSocket (`frontend/api.ts`)
**In `StatusWebSocket.connect()` method:**
```typescript
this.ws.onopen = () => {
  // Send auth message first
  const token = getAuthToken();
  if (token) {
    this.ws!.send(JSON.stringify({ type: 'auth', token }));
  }
  this.onConnectionCallback?.(true);
};
```

#### D. Create Login Component (`frontend/Login.tsx`)
**New file:**
```typescript
import React, { useState } from 'react';
import { startLogin } from './api';

const Login: React.FC = () => {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleLogin = async () => {
    setLoading(true);
    setError(null);

    try {
      const { authorization_url } = await startLogin();
      window.location.href = authorization_url;
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Login failed');
      setLoading(false);
    }
  };

  return (
    <div className="min-h-screen flex items-center justify-center bg-zinc-950">
      <div className="bg-zinc-900 p-8 rounded-xl border border-zinc-800 max-w-md w-full text-center">
        <h1 className="text-2xl font-bold text-zinc-100 mb-2">Office Climate</h1>
        <p className="text-zinc-400 mb-6">
          Sign in with your authorized Google account
        </p>

        {error && (
          <div className="mb-4 p-3 bg-red-500/10 border border-red-500 rounded text-red-400 text-sm">
            {error}
          </div>
        )}

        <button
          onClick={handleLogin}
          disabled={loading}
          className="w-full py-3 px-6 bg-blue-600 hover:bg-blue-700 disabled:bg-blue-800 disabled:cursor-not-allowed text-white font-medium rounded-lg transition-colors"
        >
          {loading ? 'Redirecting...' : 'Sign in with Google'}
        </button>
      </div>
    </div>
  );
};

export default Login;
```

#### E. Update App Component (`frontend/App.tsx`)
**Add imports:**
```typescript
import Login from './Login';
import { isAuthenticated, logout, getUserEmail } from './api';
```

**Add state:**
```typescript
const [authenticated, setAuthenticated] = useState(isAuthenticated());
const [userEmail, setUserEmail] = useState(getUserEmail());
```

**Add logout handler:**
```typescript
const handleLogout = async () => {
  await logout();
  setAuthenticated(false);
  setUserEmail(null);
};
```

**Show login screen:**
```typescript
// At the top of the render, before the dashboard
if (!authenticated) {
  return <Login />;
}
```

**Add logout button to header:**
```typescript
<div className="flex items-center gap-3">
  <span className="text-sm text-zinc-400">{userEmail}</span>
  <button
    onClick={handleLogout}
    className="text-xs bg-zinc-800 hover:bg-zinc-700 text-zinc-300 px-3 py-1 rounded transition-colors"
  >
    Logout
  </button>
</div>
```

### 4. Occupancy Detector Update (✅ Complete - Commit 72dcc48)

**File**: `occupancy_detector.py` - All changes implemented

**Add import** (after line 26):
```python
from oauth_device_client import OAuthDeviceClient
```

**Update `send_to_orchestrator` signature** (line 117):
```python
def send_to_orchestrator(
    state: OccupancyState,
    orchestrator_url: str,
    oauth_client: Optional[OAuthDeviceClient] = None,
    # Legacy Basic Auth (deprecated)
    auth_username: Optional[str] = None,
    auth_password: Optional[str] = None
) -> bool:
```

**Update authentication logic** (replace lines 135-167):
```python
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
```

**Update main function** (add OAuth setup before watch loop):
```python
# Add command-line args
parser.add_argument("--auth-token-file", type=str, help="OAuth token file path")
parser.add_argument("--reauth", action="store_true", help="Force re-authentication")

args = parser.parse_args()

# Set up OAuth client
oauth_client = None
if args.url:
    token_file = Path(args.auth_token_file) if args.auth_token_file else None
    oauth_client = OAuthDeviceClient(args.url, token_file)

    # Load existing token or authenticate
    if args.reauth or not oauth_client.load_token():
        if not oauth_client.authenticate():
            print("Authentication failed", file=sys.stderr)
            sys.exit(1)

# Update send_to_orchestrator calls
send_to_orchestrator(
    state,
    args.url,
    oauth_client=oauth_client,
    auth_username=args.auth_username,
    auth_password=args.auth_password
)
```

---

## Google Cloud Console Setup

**Before testing, you must:**

1. Visit https://console.cloud.google.com
2. Create project: "Office Climate Automation"
3. Enable "Google+ API" (or "Google Identity Services")
4. Create OAuth 2.0 Client ID:
   - **Type**: Web application
   - **Authorized redirect URIs**:
     - `https://climate.loca.lt/auth/callback`
     - `http://192.168.5.140:8080/auth/callback`
     - `http://localhost:8080/auth/callback`
5. Copy Client ID and Client Secret

6. **Update `config.yaml`**:
   ```yaml
   orchestrator:
     google_oauth:
       client_id: "YOUR_CLIENT_ID.apps.googleusercontent.com"
       client_secret: "YOUR_CLIENT_SECRET"
       allowed_emails:
         - "rajeshgoli+kumo@gmail.com"
   ```

---

## Testing Checklist

### Implementation Status
- [x] Backend OAuth service (commit a728dbd)
- [x] Frontend authentication (commit 72dcc48)
- [x] Occupancy detector OAuth (commit 72dcc48)
- [x] All code pushed to origin/main

### 🧪 Ready for Local Testing (Requires Google OAuth Setup)

**Prerequisites:**
1. Set up Google Cloud Console OAuth credentials (see below)
2. Update config.yaml with client_id and client_secret

**Frontend Testing:**
- [ ] Visit http://localhost:5173 shows login screen
- [ ] Click "Sign in with Google" redirects to Google
- [ ] Approve access with rajeshgoli+kumo@gmail.com
- [ ] Redirected back, JWT stored in localStorage
- [ ] Dashboard loads with real-time data
- [ ] WebSocket connects successfully
- [ ] Manual controls (ERV, HVAC) work
- [ ] Logout clears token and shows login

**Detector Testing:**
- [ ] Run: `python3 occupancy_detector.py --url http://localhost:8080 --reauth`
- [ ] See device code and verification URL
- [ ] Visit URL on phone, enter code
- [ ] Approve with rajeshgoli+kumo@gmail.com
- [ ] Detector shows "Authorization successful"
- [ ] Token saved to `~/.office-automate/auth_token.json`
- [ ] Run again without `--reauth`, uses saved token
- [ ] POST `/occupancy` succeeds (no more 401 errors)

---

## Deployment Steps (After Local Testing)

### 1. Frontend Build
```bash
cd frontend
npm run build
```

### 2. Deploy to Mac Mini
```bash
# Deploy backend
ssh rajesh@192.168.5.140
cd ~/office-automate
git pull
source venv/bin/activate
pip install google-auth google-auth-oauthlib PyJWT cryptography

# Update config.yaml with OAuth credentials

# Deploy frontend
exit
scp -r frontend/dist/* rajesh@192.168.5.140:~/office-automate/frontend/dist/

# Restart services
ssh rajesh@192.168.5.140
launchctl unload ~/Library/LaunchAgents/com.office-automate.orchestrator.plist
launchctl load ~/Library/LaunchAgents/com.office-automate.orchestrator.plist
```

### 3. Authenticate Occupancy Detector
```bash
# On work Mac
python3 ~/Desktop/office-automate/occupancy_detector.py \
  --url http://192.168.5.140:8080 \
  --reauth

# Follow device flow on phone
# Visit https://www.google.com/device
# Enter displayed code
# Approve with rajeshgoli+kumo@gmail.com
```

### 4. Update Launch Agent
Edit `~/Library/LaunchAgents/com.office-automate.occupancy.plist`:

**Replace:**
```xml
<string>--auth-username</string>
<string>rajesh</string>
<string>--auth-password</string>
<string>noklpd1!</string>
```

**With:**
```xml
<string>--auth-token-file</string>
<string>/Users/rajesh/.office-automate/auth_token.json</string>
```

**Reload:**
```bash
launchctl unload ~/Library/LaunchAgents/com.office-automate.occupancy.plist
launchctl load ~/Library/LaunchAgents/com.office-automate.occupancy.plist
```

---

## Architecture Summary

```
Browser → GET /auth/login
       ← {authorization_url: "https://accounts.google.com/..."}
       → Redirect to Google OAuth
       ← Google callback: GET /auth/callback?code=...&state=...
       ← HTML: localStorage.setItem('auth_token', JWT)
       → GET /status (Authorization: Bearer JWT)
       ← Status data

WebSocket → WS /ws
          → {"type": "auth", "token": JWT}
          ← Status updates

Detector → POST /auth/device/start
        ← {device_code, user_code, verification_url}
        → [User approves on phone]
        → POST /auth/device/poll {device_code}
        ← {status: "success", access_token: JWT, ...}
        → POST /occupancy (Authorization: Bearer JWT)
```

---

## Rollback Plan

If OAuth causes issues:

1. **Comment out OAuth in config.yaml:**
   ```yaml
   orchestrator:
     # google_oauth:
     #   client_id: "..."
     auth_username: "rajesh"
     auth_password: "noklpd1!"
   ```

2. **Restart orchestrator** - automatically falls back to Basic Auth

3. **Restore detector Launch Agent** - add back `--auth-username` / `--auth-password`

---

## Next Steps

1. ✅ ~~Implement frontend changes~~ (COMPLETE - commit 72dcc48)
2. ✅ ~~Update occupancy detector~~ (COMPLETE - commit 72dcc48)
3. ⏳ **Set up Google Cloud Console** (OAuth credentials - see section above)
4. ⏳ **Test end-to-end flow** (browser + detector - requires OAuth setup)
5. ⏳ **Deploy to Mac Mini** (build frontend, update configs)
6. ⏳ **Update CLAUDE.md** (document OAuth as primary auth)

---

## Key Files Reference

**Backend (✅ Complete - commit a728dbd):**
- `src/oauth_service.py` - Core OAuth logic
- `src/orchestrator.py` - OAuth endpoints (lines 1103-1222), JWT middleware (lines 1064-1101), WebSocket auth (lines 982-1008)
- `src/config.py` - GoogleOAuthConfig (lines 77-83)
- `oauth_device_client.py` - Device flow client

**Frontend (✅ Complete - commit 72dcc48):**
- `frontend/api.ts` - Token management, Bearer auth headers, 401 handling
- `frontend/Login.tsx` - NEW FILE - Login screen with Google OAuth
- `frontend/App.tsx` - Auth state, user email display, logout button

**Detector (✅ Complete - commit 72dcc48):**
- `occupancy_detector.py` - OAuth device flow integration, --reauth flag

**Config (⏳ Needs OAuth Credentials):**
- `config.yaml` - Add Google OAuth client_id and client_secret

---

## Notes

- **JWT Secret**: Auto-generated on first run, stored in memory. Will change on restart (users re-login). Set `jwt_secret` in config.yaml for consistency.
- **Session Storage**: In-memory Dict[email, UserSession]. Lost on restart. Acceptable for single-user system.
- **Token Expiry**: 7 days. No refresh tokens for browser. Device flow tokens refresh automatically.
- **Email Allowlist**: Hardcoded to rajeshgoli+kumo@gmail.com. Add more emails to `allowed_emails` list if needed.
- **Backwards Compatibility**: Basic Auth still works if OAuth not configured. Safe to deploy backend now.

---

**Commits**:
- a728dbd - Backend OAuth implementation
- 72dcc48 - Frontend & detector OAuth integration
- 88645f6 - Documentation updates

**Branch**: main
**Status**: ✅ All code complete and pushed to origin

**Next**: Set up Google OAuth credentials → Test locally → Deploy to Mac Mini
