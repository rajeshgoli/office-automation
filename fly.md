# Fly.io Tunnel Setup for Office Climate

⚠️ **DEPRECATED:** This approach is more complex than needed. See `tailscale.md` for the simpler solution.

---

Get `climate.rajeshgo.li` working from anywhere using Fly.io.

---

## Prerequisites

- Fly.io account (you already have one)
- `flyctl` installed: `brew install flyctl`

---

## Steps (Do Tomorrow)

### 1. Login to Fly.io

```bash
flyctl auth login
```

This opens a browser - just authorize it.

---

### 2. Create WireGuard tunnel (one-time setup)

```bash
flyctl wireguard create
```

Follow the prompts:
- Region: Choose closest to you (e.g., `sjc` for San Jose)
- Name: `office-climate-tunnel` (or whatever)

**IMPORTANT:** Copy the IPv6 address it gives you! It'll look like:
```
fdaa:0:1234:a7b:abc:0:a:2
```

You need this for the next step.

---

### 3. Update Caddyfile with your WireGuard IP

Edit `~/Desktop/office-automate/fly-proxy/Caddyfile`:

Replace `YOUR_WIREGUARD_IP` with the IPv6 address from step 2.

Example:
```
reverse_proxy http://[fdaa:0:1234:a7b:abc:0:a:2]:8080
```

---

### 4. Deploy the proxy app

Files are already created in `fly-proxy/`. Just run:

```bash
cd ~/Desktop/office-automate/fly-proxy
flyctl launch --no-deploy
```

When prompted:
- App name: `climate-proxy` (or whatever you want)
- Region: Same as step 2
- Database: **No**
- Redis: **No**

Then deploy:

```bash
flyctl deploy
```

---

### 5. Start your backend

Make sure the orchestrator is running on your Mac:

```bash
cd ~/Desktop/office-automate
source venv/bin/activate
python run.py
```

Keep this running in a separate terminal.

---

### 6. Add DNS record

At your DNS provider (NameCheap or wherever rajeshgo.li is):

Add CNAME record:
- **Name:** `climate`
- **Value:** `climate-proxy.fly.dev` (replace `climate-proxy` with whatever app name you chose)
- **TTL:** Automatic or 300

Wait 5-10 minutes for DNS to propagate.

---

### 7. Test it!

1. Visit `https://climate.rajeshgo.li` in Safari on iPhone
2. Should see your dashboard with live data
3. Tap Share → "Add to Home Screen"
4. Done!

---

## Moving to Raspberry Pi (Later)

When Pi is ready:

1. Copy project to Pi
2. Install orchestrator on Pi (same as Mac setup)
3. Install flyctl on Pi:
   ```bash
   curl -L https://fly.io/install.sh | sh
   ```
4. Login on Pi:
   ```bash
   flyctl auth login
   ```
5. Create new WireGuard connection from Pi (or transfer the existing one)
6. Update Caddyfile with Pi's WireGuard IP
7. Redeploy: `flyctl deploy`
8. Backend now runs on Pi 24/7

---

## Troubleshooting

**"Connection refused"**
- Make sure orchestrator is running (`python run.py`)
- Check WireGuard is connected: `flyctl wireguard list`

**"DNS not resolving"**
- Wait 5-10 minutes for DNS propagation
- Check CNAME was added correctly at your DNS provider
- Try: `dig climate.rajeshgo.li`

**"502 Bad Gateway"**
- Restart orchestrator
- Check Caddyfile has correct WireGuard IP
- Redeploy proxy: `cd fly-proxy && flyctl deploy`

**Dashboard loads but no data**
- Check orchestrator logs for errors
- Verify all sensors are connected (YoLink, Qingping, ERV)

---

## Cost

- **Free tier:** 3 shared-cpu VMs + 160GB transfer/month
- This tiny proxy uses minimal resources, should stay free
- WireGuard tunnel is free

---

## Authentication (Optional)

To add password protection, edit `config.yaml`:

```yaml
orchestrator:
  host: "0.0.0.0"
  port: 8080
  auth_username: "admin"
  auth_password: "your-secure-password"
```

Then restart orchestrator. Browser will prompt for login.
