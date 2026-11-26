#!/bin/sh
# `trellis rules check` for ./tools/verify on CI (CI=true), run from the checkout root.
#
# A CI runner starts the trellis daemon cold, so this checkout's first index and architecture
# build can outlast the CLI's own waits (90 s and 30 s). trellis then answers with a typed
# index_not_ready or deadline_exceeded, keeps building, and answers a retry. classify.py
# recognises exactly those two; this retries them, at most 4 attempts in all. A deviation and
# every other failure fail at once with trellis's exit status, as they do locally.
set -u

here=$(cd "$(dirname "$0")" && pwd)
attempts=4
dir=$(mktemp -d "${TMPDIR:-/tmp}/combine-trellis.XXXXXX") || exit 1
trap 'rm -rf "$dir"' EXIT

attempt=1
while :; do
  status=0
  trellis rules check --format json >"$dir/report.json" 2>"$dir/stderr" || status=$?
  cat "$dir/stderr" >&2
  [ "$status" -eq 0 ] && exit 0
  if [ "$attempt" -ge "$attempts" ]; then
    echo "verify: trellis was still building after $attempts attempts" >&2
    exit "$status"
  fi
  code=$(python3 -I -B "$here/classify.py" "$status" "$dir/report.json" "$dir/stderr") || exit "$status"
  echo "verify: trellis answered $code (attempt $attempt of $attempts); retrying in 20 s" >&2
  attempt=$((attempt + 1))
  sleep 20
done
