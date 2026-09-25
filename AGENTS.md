# 1. Working in this repo

I am Rajesh, I own the repo and am the only human on it. This repo is office climate automation system for a backyard shed office. Coordinates smart devices to maintain air quality silently during occupancy and aggressively ventilate when away.   

This file is the whole standing contract. Read all of it.

## 2. Your workflow

**Name yourself.** Before you begin work, check your name with `sm me`. If it is `claude-<slug>`, `codex-fork-<slug>`, or anything similar, replace it with `sm name oa-<newname>`. `oa-<ticket>-engineer`, `oa-<ticket>-scout`, `oa-<spec-section>-engineer`, `oa-<pr>-spec-repair`, `oa-<ticket>-spec-author`, `oa-<ticket>-spec-reviewer` and `oa-<pr>-reviewer-<round>` all beat `claude-<slug>`, because a name that says what you were doing is what lets me restore you. The `oa` prefix allows me to know you're working on a office automation as opposed to my primary repo without needing to dig deeper.

**Worktrees.** Every agent works in its own worktree under `~/worktrees/oa-<ticket>-<slug>`, build outputs inside. 

**Claim your work.** Start a ticket with `sm ticket <N> --setup-worktree` and work in the worktree it prints (plain `sm ticket <N>` if you already have one). When you open a PR, or take over someone else's, run `sm pr` from its branch. Put `Closes #<N>` in
the PR body for each ticket the PR finishes; "Implements #N" does not close it. Reviewers and scouts don't claim. When you are retired, sm deletes your worktree if nothing would be lost; if something there must outlive you (a running server, results not yet pushed), run `sm worktree keep --reason "<why>"`.

 Workflow as usual:
 1. Rebuild and restart office automation server as required. If it's pure android app update, you don't need to restart server, otherwise you may need to.  
 2. If I need to test something let me know. For example, if something can be tested with android app, let me know exactly what to try out.  If you can test directly that's preferred. For example, if you can reliable reproduce the issue I reported and you can verify it no longer occurs, you can tell me what you did and ask me to try it optionally.
 3. Once all feature requests above are completed and verified, you may exit to step 4. If I have feedback or if you find live test failures, repeat steps 1 and 2 until exit to 4 criteria is met. 
 4. Once functionality is in place, create a PR for your changes.
 5. Use instructions in Review loop section to get your PR in a clean mergable state.
 6. Once clean, squash merge the PR, delete local and remote branches or worktrees you may have created.
 7. Run `cargo clean && scripts/build-server.sh && cargo clean --target-dir target-signing`, then restart the server. Run all three in one go, because the first clean deletes the deployed binary.
 8. Let me know.
    
## 4. Review loop
Request a review with `sm request-codex-review <pr-number>`. Treat the response as registration only, then go idle — do not poll. If Session Manager cannot take the request, post `@codex review` as a PR comment, check back after five minutes, again after five more. If codex hasn't acknowledged your review after 10 minutes with 👀 smiley, you can re-post the request. If nothing has landed after 20 minutes, you can re-post the review request.

Before acting on a review, confirm it belongs to your current request and was posted after your latest push. A review existing is not enough on its own.

Then:

1. **Classify every finding: valid, partially valid, or invalid.** Do not skip this. A review is not gospel — push back with reasoning where it is wrong.
2. **Correctness only** — no document nits, no wording preferences, no nits about following process for process's sake. That excludes process *preference*, not process *correctness*: when the thing under review is a workflow, an instruction file, CI, or a deploy step, its behaviour is the correctness surface, and a defect in it is a correctness finding however procedural it sounds. "This deploys without re-running the tests" is a bug, not a nit.
3. **Any unresolved P1 blocks.** A P1 is resolved either by fixing it or by answering it: a P1 you classify invalid, with your reasoning posted on the PR, is resolved and does not block. It is unresolved only while it is neither fixed nor answered — otherwise a single false positive strands the PR forever, since there is no code change to push for the next round. If the reviewer re-raises the same P1 after reading your reasoning, that is a real disagreement: escalate it rather than looping. A round that returns only P2 or lower, or a clean review, exits the loop. Do not keep chasing P2s and P3s.  
5. Fix, push, and re-review at the exact head. Fewest rounds to correctness — which does not mean dropping correctness issues.



## 3. Repo reference 

- `rust/office-automate-server/` - Rust server, collectors, device clients, and CLI
- `rust/office-automate-server/src/http.rs` - HTTP/WS server + OAuth endpoints
- `rust/office-automate-server/src/state.rs` - PRESENT/AWAY state logic
- `rust/office-automate-server/src/erv.rs` - ERV control via Tuya local
- `rust/office-automate-server/src/hvac.rs` - Mitsubishi Kumo Cloud (HVAC)
- `rust/office-automate-server/src/yolink.rs` - YoLink cloud API (sensors)
- `rust/office-automate-server/src/qingping.rs` - Qingping MQTT client (air quality)
- `frontend/` - React + Vite dashboard
- `android/` - Android client

```bash
# Backend
cargo run --manifest-path rust/office-automate-server/Cargo.toml -- serve --config config.yaml

# Frontend
cd frontend
VITE_API_PORT=8080 npm run dev -- --port 9002
```

SQLite Db at `data/office_climate.db`. Tables: `sensor_readings`, `occupancy_log`, `device_events`, `climate_actions`. All timestamps in local time (PST).
