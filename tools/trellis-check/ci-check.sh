#!/bin/sh
# `trellis rules check` for ./tools/verify on CI (CI=true), run from the checkout root.
#
# A CI runner starts the trellis daemon cold, so this checkout's first index and architecture
# build can outlast the CLI's own waits (90 s and 30 s). trellis then answers with a typed
# index_not_ready or deadline_exceeded, keeps building, and answers a retry. classify.py
# recognises exactly those two; this retries them, at most 4 attempts in all. A deviation and
# every other failure fail at once with trellis's exit status, as they do locally. A report
# trellis printed is summarized (summary.py); the summary never changes the exit status.
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
  if [ "$status" -eq 0 ]; then
    python3 -I -B "$here/summary.py" "$dir/report.json" || :
    exit 0
  fi
  if ! code=$(python3 -I -B "$here/classify.py" "$status" "$dir/report.json" "$dir/stderr"); then
    # A final answer, such as a deviation: summarize the report if trellis printed one.
    if [ -s "$dir/report.json" ]; then
      python3 -I -B "$here/summary.py" "$dir/report.json" || :
    fi
    exit "$status"
  fi
  if [ "$attempt" -ge "$attempts" ]; then
    echo "verify: trellis was still building after $attempts attempts" >&2
    exit "$status"
  fi
  echo "verify: trellis answered $code (attempt $attempt of $attempts); retrying in 20 s" >&2
  attempt=$((attempt + 1))
  sleep 20
done
