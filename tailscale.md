# Tailscale Funnel Setup for Office Climate

Get `climate.rajeshgo.li` working from anywhere using Tailscale Funnel (free, super simple).

---

## Prerequisites

- Old Mac Mini (or Pi, or any always-on device)
- Tailscale account (free personal plan)

---

## Steps (Do on Mac Mini)

### 1. Install Tailscale

On the Mac Mini:

```bash
brew install tailscale
```

Or download from: https://tailscale.com/download/mac

---

### 2. Login to Tailscale

```bash
sudo tailscale up
```

Opens browser - login with your Google/GitHub/Microsoft account.

---

### 3. Set up the orchestrator on Mac Mini

Copy the project:

```bash
# On your development Mac
rsync -av --exclude 'data/' --exclude 'node_modules/' \
  ~/Desktop/office-automate/ macmini.local:~/office-automate/
```

On Mac Mini:

```bash
cd ~/office-automate
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt

# Install Mosquitto for Qingping
brew install mosquitto
brew services start mosquitto

# Copy your config
cp config.yaml.example config.yaml
# Edit config.yaml with your credentials (nano config.yaml)

# Test it
python run.py
```

Verify it works at `http://localhost:8080`

---

### 4. Enable Tailscale Funnel

On Mac Mini:

```bash
tailscale funnel 8080
```

This will give you a public HTTPS URL like:
```
https://macmini.your-tailnet.ts.net
```

**Copy this URL!** You need it for the next step.

Leave this running (or set it up as a service in step 6).

---

### 5. Add DNS CNAME

At your DNS provider (NameCheap or wherever rajeshgo.li is):

Add CNAME record:
- **Name:** `climate`
- **Value:** `macmini.your-tailnet.ts.net` (from step 4, without `https://`)
- **TTL:** Automatic or 300

Wait 5-10 minutes for DNS to propagate.

---

### 6. Set up as launch agent (runs on startup)

On Mac Mini, create launch agent:

```bash
mkdir -p ~/Library/LaunchAgents
nano ~/Library/LaunchAgents/com.office-climate.orchestrator.plist
```

Paste this (update paths if needed):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.office-climate.orchestrator</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/YOURUSERNAME/office-automate/venv/bin/python</string>
        <string>/Users/YOURUSERNAME/office-automate/run.py</string>
    </array>
    <key>WorkingDirectory</key>
    <string>/Users/YOURUSERNAME/office-automate</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/Users/YOURUSERNAME/office-automate/orchestrator.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/YOURUSERNAME/office-automate/orchestrator.error.log</string>
</dict>
</plist>
```

Replace `YOURUSERNAME` with your Mac Mini username.

Load it:

```bash
launchctl load ~/Library/LaunchAgents/com.office-climate.orchestrator.plist
```

For Tailscale Funnel, create another one:

```bash
nano ~/Library/LaunchAgents/com.office-climate.funnel.plist
```

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.office-climate.funnel</string>
    <key>ProgramArguments</key>
    <array>
        <string>/opt/homebrew/bin/tailscale</string>
        <string>funnel</string>
        <string>8080</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
```

Load it:

```bash
launchctl load ~/Library/LaunchAgents/com.office-climate.funnel.plist
```

Now both will start automatically on boot!

---

### 7. Add authentication (Optional but recommended)

Edit `config.yaml` on Mac Mini:

```yaml
orchestrator:
  host: "0.0.0.0"
  port: 8080
  auth_username: "admin"
  auth_password: "your-secure-password-here"
```

Restart orchestrator:

```bash
launchctl unload ~/Library/LaunchAgents/com.office-climate.orchestrator.plist
launchctl load ~/Library/LaunchAgents/com.office-climate.orchestrator.plist
```

Now anyone accessing the site needs the password.

---

### 8. Update Mac occupancy detector

On your **work Mac** (in the office), update the occupancy detector to POST to the Mac Mini:

```bash
python3 occupancy_detector.py --watch --url http://macmini.local:8080
```

Or use the Tailscale URL if Mac Mini is on a different network.

---

### 9. Test it!

1. Visit `https://climate.rajeshgo.li` in Safari on iPhone
2. Enter password if you set one up
3. Should see your dashboard with live data
4. Tap Share → "Add to Home Screen"
5. Done!

---

## Troubleshooting

**"DNS not resolving"**
- Wait 10 minutes for DNS propagation
- Check CNAME: `dig climate.rajeshgo.li`
- Make sure you used the hostname only (no `https://`)

**"Connection refused"**
- Check orchestrator is running: `curl http://localhost:8080/status`
- Check Tailscale Funnel: `tailscale funnel status`
- Check logs: `tail -f ~/office-automate/orchestrator.log`

**"Can't connect from outside"**
- Tailscale Funnel requires HTTPS (not HTTP)
- Make sure Mac Mini isn't sleeping (System Preferences → Energy Saver → Prevent sleeping)

**Mac Mini goes to sleep**
```bash
sudo pmset -a sleep 0
sudo pmset -a disablesleep 1
```

---

## Cost

**Free!** Tailscale personal plan includes:
- Up to 100 devices
- Unlimited Funnel usage
- HTTPS included

---

## Why this is better than Fly.io/Cloudflare

- ✅ Simpler setup (5 steps vs 7)
- ✅ No Docker/containers needed
- ✅ No DNS provider changes
- ✅ Stable URL that doesn't expire
- ✅ Built-in HTTPS
- ✅ Mac Mini is more reliable than keeping dev Mac running
- ✅ Everything stays on your network (data never leaves Tailscale tunnel)
