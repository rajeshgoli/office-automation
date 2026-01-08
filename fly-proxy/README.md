# Fly.io Proxy App

This directory contains a simple Caddy reverse proxy that forwards requests from `climate.rajeshgo.li` to your Mac/Pi via WireGuard tunnel.

## Files

- **`Dockerfile`** - Builds a Caddy container with your config
- **`Caddyfile`** - Reverse proxy configuration (you'll update with your WireGuard IP)
- **`fly.toml`** - Fly.io app configuration

## Setup

See `../fly.md` for complete setup instructions.

## How it works

```
Internet
    ↓
climate.rajeshgo.li (DNS CNAME)
    ↓
Fly.io proxy app (this)
    ↓ (WireGuard tunnel)
Your Mac/Pi :8080 (orchestrator)
```

The proxy runs on Fly.io's edge network and forwards all traffic through a private WireGuard tunnel to your home network.
