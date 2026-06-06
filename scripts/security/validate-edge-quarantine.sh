#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: validate-edge-quarantine.sh --tunnel-user USER [options]

Validates the local public-edge quarantine boundary by running read and network
checks as the configured tunnel/edge users.

Required:
  --tunnel-user USER              Dedicated cloudflared launchd user.

Optional:
  --edge-user USER                Public HTTP edge user. Defaults to tunnel user.
  --protected-path PATH           Path tunnel/edge users must not read or traverse. Repeatable.
  --tunnel-readable PATH          Path tunnel user must read, usually cloudflared config/creds. Repeatable.
  --tunnel-private PATH           Path edge user must not read or traverse. Repeatable.
  --edge-readable PATH            Path edge user must read. Repeatable.
  --edge-private PATH             Path tunnel user must not read or traverse. Repeatable.
  --lan-probe HOST:PORT           LAN/RFC1918 endpoint tunnel/edge users must not reach. Repeatable.
  --lan-control-user USER         User that must reach LAN probes before denial checks. Defaults to current user.
  --origin-probe HOST:PORT        Loopback origin endpoint tunnel/edge users may reach. Repeatable.

The script requires passwordless sudo for `sudo -n -u USER ...` checks.
USAGE
}

tunnel_user=""
edge_user=""
protected_paths=()
tunnel_readable_paths=()
tunnel_private_paths=()
edge_readable_paths=()
edge_private_paths=()
lan_probes=()
lan_control_user=""
origin_probes=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tunnel-user)
      tunnel_user="${2:-}"
      shift 2
      ;;
    --edge-user)
      edge_user="${2:-}"
      shift 2
      ;;
    --protected-path)
      protected_paths+=("${2:-}")
      shift 2
      ;;
    --tunnel-readable)
      tunnel_readable_paths+=("${2:-}")
      shift 2
      ;;
    --tunnel-private)
      tunnel_private_paths+=("${2:-}")
      shift 2
      ;;
    --edge-readable)
      edge_readable_paths+=("${2:-}")
      shift 2
      ;;
    --edge-private)
      edge_private_paths+=("${2:-}")
      shift 2
      ;;
    --lan-probe)
      lan_probes+=("${2:-}")
      shift 2
      ;;
    --lan-control-user)
      lan_control_user="${2:-}"
      shift 2
      ;;
    --origin-probe)
      origin_probes+=("${2:-}")
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$tunnel_user" ]]; then
  echo "--tunnel-user is required" >&2
  usage >&2
  exit 2
fi
if [[ -z "$edge_user" ]]; then
  edge_user="$tunnel_user"
fi

run_as() {
  local user="$1"
  shift
  sudo -n -u "$user" "$@"
}

require_command() {
  local command_name="$1"
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "required command not found: $command_name" >&2
    exit 2
  fi
}

ensure_user_runnable() {
  local user="$1"
  if ! run_as "$user" true; then
    echo "cannot run checks as user=$user; passwordless sudo may be missing" >&2
    exit 2
  fi
}

expect_readable() {
  local user="$1"
  local path="$2"
  if [[ -z "$path" ]]; then
    echo "empty readable path" >&2
    exit 2
  fi
  run_as "$user" test -r "$path"
  echo "PASS readable: user=$user path=$path"
}

expect_unreadable() {
  local user="$1"
  local path="$2"
  if [[ -z "$path" ]]; then
    echo "empty protected path" >&2
    exit 2
  fi
  if run_as "$user" test -r "$path"; then
    echo "FAIL readable by quarantined user: user=$user path=$path" >&2
    exit 1
  fi
  if run_as "$user" test -x "$path"; then
    echo "FAIL traversable/executable by quarantined user: user=$user path=$path" >&2
    exit 1
  fi
  echo "PASS unreadable and non-traversable: user=$user path=$path"
}

split_host_port() {
  local endpoint="$1"
  local __host_var="$2"
  local __port_var="$3"
  if [[ "$endpoint" != *:* ]]; then
    echo "endpoint must be HOST:PORT: $endpoint" >&2
    exit 2
  fi
  printf -v "$__host_var" '%s' "${endpoint%:*}"
  printf -v "$__port_var" '%s' "${endpoint##*:}"
}

expect_connect() {
  local user="$1"
  local endpoint="$2"
  local host port
  split_host_port "$endpoint" host port
  run_as "$user" nc -G 2 -z "$host" "$port"
  echo "PASS connect allowed: user=$user endpoint=$endpoint"
}

expect_control_connect() {
  local endpoint="$1"
  local host port
  split_host_port "$endpoint" host port
  if [[ -n "$lan_control_user" ]]; then
    if ! run_as "$lan_control_user" nc -G 2 -z "$host" "$port"; then
      echo "FAIL LAN control is not reachable: user=$lan_control_user endpoint=$endpoint" >&2
      exit 1
    fi
    echo "PASS LAN control reachable: user=$lan_control_user endpoint=$endpoint"
  else
    if ! nc -G 2 -z "$host" "$port"; then
      echo "FAIL LAN control is not reachable: user=current endpoint=$endpoint" >&2
      exit 1
    fi
    echo "PASS LAN control reachable: user=current endpoint=$endpoint"
  fi
}

expect_no_connect() {
  local user="$1"
  local endpoint="$2"
  local host port
  split_host_port "$endpoint" host port
  if run_as "$user" nc -G 2 -z "$host" "$port"; then
    echo "FAIL LAN/RFC1918 connect allowed: user=$user endpoint=$endpoint" >&2
    exit 1
  fi
  echo "PASS connect denied: user=$user endpoint=$endpoint"
}

require_command sudo
require_command nc
ensure_user_runnable "$tunnel_user"
ensure_user_runnable "$edge_user"
if [[ -n "$lan_control_user" ]]; then
  ensure_user_runnable "$lan_control_user"
fi

for path in "${protected_paths[@]}"; do
  expect_unreadable "$tunnel_user" "$path"
  expect_unreadable "$edge_user" "$path"
done

for path in "${tunnel_readable_paths[@]}"; do
  expect_readable "$tunnel_user" "$path"
done

for path in "${edge_readable_paths[@]}"; do
  expect_readable "$edge_user" "$path"
done

for path in "${tunnel_private_paths[@]}"; do
  expect_unreadable "$edge_user" "$path"
done

for path in "${edge_private_paths[@]}"; do
  expect_unreadable "$tunnel_user" "$path"
done

for endpoint in "${origin_probes[@]}"; do
  expect_connect "$tunnel_user" "$endpoint"
  expect_connect "$edge_user" "$endpoint"
done

for endpoint in "${lan_probes[@]}"; do
  expect_control_connect "$endpoint"
  expect_no_connect "$tunnel_user" "$endpoint"
  expect_no_connect "$edge_user" "$endpoint"
done
