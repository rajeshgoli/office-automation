# Office Automate

**The honest answer to "did I actually work today?" — and the system that makes that answer possible.**

Office Automate runs on a Mac Mini in the corner of your office. It looks like two products and is really one: a climate system that knows when you're in the room, and a productivity tracker that uses the same signal to keep itself honest. Neither half works alone — and that's exactly the point.

---

## Why these two things belong together

Every "track your productivity" tool has the same broken assumption: that you tell it when you worked. So you either lie to yourself, or you forget, or you spend more time logging the work than doing it.

Every "smart office" tool has the opposite problem: it knows you're in the room, but it doesn't care what you were doing there.

Productivity tracking that's worth anything has to answer two questions at once:

1. **Were you actually here?** Not "your laptop was on" — *here*, in the chair, with the door closed, breathing the air. That's a sensor problem: door sensors, motion, keyboard activity, external monitor, CO2 rising because a human is exhaling into the room.
2. **What did you do while you were here?** Git commits, Claude sessions, Codex sessions, agent dispatches, persona reads, deploys, whatever input metric actually matters for the project. That's an integration problem.

You can't do (2) without (1) or the numbers are noise. And you can't do (1) without (2) or the sensors are just running a thermostat.

Office Automate runs both. The presence state machine that decides whether the ERV should be quiet *is the same signal* that decides whether a git commit counted as office time. One stack, one source of truth.

---

## How the two halves feed each other

```
        Sensors                  State machine               Productivity ledger
   ┌──────────────┐         ┌──────────────────┐         ┌─────────────────────┐
   │ Door / window│         │                  │         │ git commits         │
   │ Motion       │────────▶│  PRESENT / AWAY  │────────▶│ Claude sessions     │
   │ Mac activity │         │  door-open mode  │  time   │ Codex sessions      │
   │ CO2 / tVOC   │         │  settle windows  │  in     │ sm dispatches/sends │
   └──────────────┘         └────────┬─────────┘  seat   │ engram folds        │
                                     │                   │ persona reads       │
                                     ▼                   │ automation events   │
                            ┌──────────────────┐         └─────────────────────┘
                            │ ERV + HVAC control│                  ▲
                            │ (hysteresis-tuned)│                  │
                            └──────────────────┘                   │
                                                     same time-in-seat signal
```

The presence state machine produces a clean timeline of when you were actually in the office. Every input metric from every project gets attributed against that timeline. So when you look at the dashboard you don't just see "you made 12 commits this week" — you see "you made 12 commits in 14 hours of actual desk time, and 9 of them landed during the morning block when CO2 was under 800 ppm."

That's the loop. Climate quality and work output, plotted against the same hours, on the same clock.

---

## What it does on the climate side

Your CO2 sensor, your ERV, and your heat pump don't know about each other. Office Automate makes them cooperate — and uses the *same* presence signal that powers the productivity ledger to decide what's appropriate right now.

- **Air quality drives ventilation, automatically.** CO2 climbs past 2000 ppm while you're working? The ERV kicks on quietly. tVOC spikes from lunch? It catches the spike even below the threshold. You leave the office? It runs a 30-minute purge, then adapts speed to how fast CO2 is actually falling, and stops when it hits outdoor baseline — instead of running forever chasing a number it can't reach.

- **Hysteresis bands prevent the cycling that wrecks every naive system.** ERV turns on at 2000 ppm and stays on until you're back to 1800. There's an away-mode settle window so a brief departure doesn't trigger a false purge. Thresholds tuned from months of real data, not picked off a forum.

- **HVAC + ERV don't sabotage each other.** When the ERV is pulling cold outside air in winter, heating suspends — no more burning gas to warm air you're about to flush. When ventilation stops, heat resumes. When a window or door opens, both stand down.

- **Presence detection that knows the difference between you sitting still and you actually leaving.** Mac activity, motion, and door events combine into a state machine that doesn't get fooled by movies, walks to grab a glass of water, or doors left open for fresh air. The same state machine the productivity view reads.

**Devices it speaks to:**
- Pioneer Airlink ERV (local Tuya, no cloud round-trip)
- Mitsubishi mini-split via Kumo Cloud
- Qingping Air Monitor (local MQTT — CO2, tVOC, PM2.5, temp, humidity, noise)
- YoLink door / window / motion sensors
- Your Mac (occupancy from keyboard / mouse + external monitor)

---

## What it does on the productivity side

You don't ship outcomes — you ship inputs that produce outcomes. Office Automate's project leverage view shows, per project, the inputs you actually put in this week, attributed against verified office time:

- **Code & ship activity** — git commits, deploys, PRs landed
- **Agent / AI session work** — Claude sessions, Codex sessions, agent dispatches, sends
- **Memory & context work** — engram fold cadence, concept growth, time since last review
- **Persona engagement** — persona reads, projects touched
- **Automation health** — events handled, state transitions

Each project plugs in its own metrics. You get a per-day, per-project view with rolling windows. No spreadsheets, no copy-pasting from terminals, no "I think I worked on that last Tuesday." You either fed the system this week, or you didn't — and because the office knew you were *in the seat*, the chart isn't lying to you about it.

It's easy to feel busy across five projects and have nothing to show on any of them. Office Automate makes the input gap visible *before* it becomes an output problem.

---

## What you see

A single dashboard, accessible from anywhere:

- **Live vitals** — CO2, temp, humidity, tVOC, PM, noise, all on tiles you can read in two seconds
- **Climate quick controls** — manual override for ERV speed and HVAC mode / setpoint, with auto-resume after timeout
- **Historical charts** — CO2 and air quality trends so you can see when your office breaks down
- **Office replay** — scrub through any past day and see exactly what the system was doing and why
- **Project leverage** — your input metrics across every project, rolling windows, per-day, attributed against real office time
- **PWA on your phone** — add to home screen on iOS, looks and behaves like a native app

Locked behind Google OAuth with an email allowlist. JWT tokens. No "admin / admin" embarrassment.

---

## What it runs on

- **A Mac Mini.** Anything that can run the Rust server binary and stay awake. Has been running quietly for months.
- **A Cloudflare tunnel** for remote access (or skip it and stay on LAN).
- **A small React dashboard** served from the same orchestrator.

That's it. No Kubernetes, no Docker, no Home Assistant. A few LaunchAgents and a SQLite file.

---

## How it's different

Home Assistant gives you a control panel. RescueTime gives you a guilt graph. Office Automate gives you the **one signal that makes both useful** — verified time in the seat — and then spends that signal on both jobs at once: keeping the room livable, and keeping the input ledger honest.

Two questions, one place, one shared source of truth:

> Is the air I'm breathing right now any good?
> Did I actually do the work this week — in the hours I was actually here?

Open the dashboard. You have your answer.
