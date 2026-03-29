#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_DIR="${REPO_ROOT}/data"
WORK_MAC_HOST="${WORK_MAC_HOST:?Set WORK_MAC_HOST to the work Mac SSH host}"
WORK_MAC_USER="${WORK_MAC_USER:-rajesh}"
CLAUDE_REMOTE="${CLAUDE_REMOTE:-~/.claude/history.jsonl}"
CODEX_REMOTE="${CODEX_REMOTE:-~/.codex/history.jsonl}"
CODEX_STATE_REMOTE="${CODEX_STATE_REMOTE:-~/.codex/state_5.sqlite}"

mkdir -p "${DATA_DIR}"

rsync -az "${WORK_MAC_USER}@${WORK_MAC_HOST}:${CLAUDE_REMOTE}" "${DATA_DIR}/claude_history.jsonl"
rsync -az "${WORK_MAC_USER}@${WORK_MAC_HOST}:${CODEX_REMOTE}" "${DATA_DIR}/codex_history.jsonl"
rsync -az "${WORK_MAC_USER}@${WORK_MAC_HOST}:${CODEX_STATE_REMOTE}" "${DATA_DIR}/codex_state.sqlite"
rsync -az "${WORK_MAC_USER}@${WORK_MAC_HOST}:~/Desktop/automation/office-automate/data/telemetry.db" "${DATA_DIR}/telemetry.db"

python3 "${REPO_ROOT}/session_stats_parser.py"
