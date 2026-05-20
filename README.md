# Office Automate

**One dashboard for the two things that actually move your week: the air you breathe and the work you ship.**

Office Automate runs on a Mac Mini in the corner of your office and quietly does two jobs at once. It keeps your climate dialed in across every device you own, and it pulls the input metrics from every project you care about into a single place so you can see, at a glance, whether you're actually moving.

---

## Why it exists

If you've ever worked out of a converted shed, garage, or back-of-the-house office, you already know the problem: your air quality, heat, and ventilation live in four different apps that don't talk to each other, and your "what did I do this week" answer lives across a dozen GitHub repos, Telegram chats, and personal projects you can't keep straight in your head.

Office Automate replaces the four apps with one. And it gives you a single productivity surface for every project you're juggling — not output vanity numbers, but the **input goals** that actually predict whether the project will work.

---

## The two things it does well

### 1. Track your input goals across every project, in one place

You don't ship outcomes — you ship inputs that produce outcomes. Office Automate's project leverage view shows you, per project, the inputs you're actually putting in this week:

- **Session activity** — dispatches, sends, active sessions, Telegram throughput
- **Memory & context work** — fold cadence, concept growth, time since last review
- **Persona engagement** — persona reads, projects touched
- **Automation health** — events handled, state transitions

Each project plugs in its own metrics. You get a per-day, per-project view in the dashboard with rolling windows. No spreadsheets, no copy-pasting from terminals, no "I think I worked on that last Tuesday." You either fed the system this week, or you didn't, and the chart shows which.

This matters because it's easy to feel busy across five projects and have nothing to show on any of them. Office Automate makes the input gap visible *before* it becomes an output problem.

### 2. Keep your office air, temperature, and ventilation under one roof — with hysteresis that actually works

Your CO2 sensor, your ERV, and your heat pump don't know about each other. Office Automate makes them cooperate.

**Smart coordination, not just smart devices:**

- **Air quality drives ventilation, automatically.** CO2 climbs past 2000 ppm while you're working? The ERV kicks on. tVOC spikes from lunch? It catches the spike even below the threshold and ventilates. You leave the office? It runs a 30-minute purge, then adapts speed to how fast the CO2 is actually falling, and stops when it hits outdoor baseline — instead of running forever chasing a number it can't reach.

- **Hysteresis bands prevent the cycling that wrecks every naive system.** ERV turns on at 2000 ppm and stays on until you're back to 1800. Heat doesn't fight ventilation. There's an away-mode settle window so a brief departure doesn't trigger a false purge. The thresholds are tuned from months of real data, not picked off a forum.

- **HVAC + ERV don't sabotage each other.** When the ERV is pulling cold outside air in winter, heating suspends — no more burning gas to warm air you're about to flush. When ventilation stops, heat resumes. When a window or door opens, both stand down.

- **Presence detection that knows the difference between you sitting still and you actually leaving.** Mac activity, motion, and door events combine into a state machine that doesn't get fooled by movies, walks to grab a glass of water, or doors left open for fresh air.

**Devices it speaks to today:**
- Pioneer Airlink ERV (local Tuya, no cloud round-trip)
- Mitsubishi mini-split via Kumo Cloud
- Qingping Air Monitor (local MQTT — CO2, tVOC, PM2.5, temp, humidity, noise)
- YoLink door/window/motion sensors
- Your Mac (occupancy from keyboard/mouse + external monitor)

---

## What you see

A single dashboard, accessible from anywhere:

- **Live vitals** — CO2, temp, humidity, tVOC, PM, noise, all on tiles you can read in two seconds
- **Climate quick controls** — manual override for ERV speed and HVAC mode/setpoint, with auto-resume after timeout
- **Historical charts** — CO2 and air quality trends so you can see when your office breaks down
- **Office replay** — scrub through any past day and see exactly what the system was doing and why
- **Project leverage** — your input metrics across every project, rolling windows, per-day
- **PWA on your phone** — add to home screen on iOS, looks and behaves like a native app

Locked behind Google OAuth with an email allowlist. JWT tokens. No "admin/admin" embarrassment.

---

## What it runs on

- **A Mac Mini.** Anything that can run Python 3.10 and stay awake. Mine is a 2014 Mac Mini on macOS High Sierra. It has been running for months.
- **A Cloudflare tunnel** for remote access (or skip it and stay on LAN).
- **A small React dashboard** served from the same orchestrator.

That's it. No Kubernetes, no Docker, no Home Assistant. Three Launch Agents and a SQLite file.

---

## How it's different

Home Assistant gives you a control panel. Office Automate gives you a *behavior*. The point isn't to toggle the ERV from your phone — it's to never have to think about the ERV again, and to know whether you actually showed up for your projects this week.

Two questions, one place:

> Is the air I'm breathing right now any good?
> Did I actually do the work this week?

Open the dashboard. You have your answer.
