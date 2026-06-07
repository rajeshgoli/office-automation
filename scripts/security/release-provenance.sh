#!/usr/bin/env bash
set -euo pipefail

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "release provenance requires a clean tracked checkout" >&2
  exit 1
fi

if [[ "$#" -eq 0 ]]; then
  set -- target/release/office-automate-server
fi

commit="$(git rev-parse HEAD)"
commit_date="$(git show -s --format=%cI HEAD)"
generated_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

echo "office-automate release provenance"
echo "generated_at=${generated_at}"
echo "git_commit=${commit}"
echo "git_commit_date=${commit_date}"
echo
echo "artifacts:"

for artifact in "$@"; do
  if [[ ! -f "${artifact}" ]]; then
    echo "missing artifact: ${artifact}" >&2
    exit 1
  fi
  size_bytes="$(wc -c <"${artifact}" | tr -d ' ')"
  sha256="$(shasum -a 256 "${artifact}" | awk '{print $1}')"
  echo "- path=${artifact}"
  echo "  size_bytes=${size_bytes}"
  echo "  sha256=${sha256}"
done
