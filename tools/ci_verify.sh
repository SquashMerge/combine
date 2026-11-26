#!/bin/sh
# The feature factory's repository `verify` (.factory.json): runs ./tools/verify on GitHub
# Actions for the commit checked out here, instead of locally under the shared heavy-run lease.
#
#   sh tools/ci_verify.sh
#
# It pushes HEAD to qualify/factory-<sha> on origin's push URL, which starts
# .github/workflows/verify.yml, then waits on the check runs GitHub records for that exact sha
# (never "the latest run on the branch"), taking the newest run of each required check. The
# qualify branch is deleted on every exit; that doesn't cancel a run still going.
#
# Exit status:
#   0   every required check is present and its newest run passed
#   1   the newest run of a required check failed
#   75  no answer (.factory.json verify_unavailable_exit): GitHub unreachable or gh not
#       authenticated, the push failed, origin's push URL names no github.com repository, no
#       required check started within CI_VERIFY_START_S of the push, the platform stopped the
#       run (cancelled, startup_failure, stale, action_required, a runner failure) more times
#       than CI_VERIFY_MAX_RERUNS fresh runs could fix, or CI_VERIFY_DEADLINE_S passed.
# The factory pauses on 75 and picks the run up again later; there is no local fallback.
#
# A required check that already has a usable run on this sha (passed, or still going) is
# waited on without a new push, so a re-run of this script reuses it. One the platform stopped
# gets a fresh run: the branch is deleted and pushed again, and only check runs newer than the
# stopped one count.
#
# Settings (environment), all whole seconds unless noted:
#   CI_VERIFY_REQUIRED          required check names, space-separated (default: verify)
#   CI_VERIFY_DEADLINE_S        give up with 75 after this long; below the factory's
#                               verify_timeout_ms of 5400000 (default: 4800, 80 minutes)
#   CI_VERIFY_START_S           how long after a push a required check may stay missing
#                               (default: 600)
#   CI_VERIFY_POLL_S            between polls (default: 30)
#   CI_VERIFY_CALL_TIMEOUT_S    bound on each git or gh call; 0 for none (default: 120)
#   CI_VERIFY_MAX_RERUNS        fresh runs after the platform stops one (default: 2)
#
# gh uses its ambient authentication (the factory driver exports GH_TOKEN); nothing here reads,
# prints or passes a token. The GitHub repository is named from origin's push URL, because in a
# factory sandbox origin fetches from a local path and gh can't infer it.
set -u

here=$(cd "$(dirname "$0")" && pwd)
evaluate="$here/ci-verify/evaluate.py"

say() { printf 'ci-verify: %s\n' "$*" >&2; }
unavailable() {
  say "no answer (exit 75): $*"
  exit 75
}

required=${CI_VERIFY_REQUIRED:-verify}
deadline_s=${CI_VERIFY_DEADLINE_S:-4800}
start_s=${CI_VERIFY_START_S:-600}
poll_s=${CI_VERIFY_POLL_S:-30}
call_s=${CI_VERIFY_CALL_TIMEOUT_S:-120}
reruns_max=${CI_VERIFY_MAX_RERUNS:-2}
poll_failures_max=5
push_attempts=3

for setting in "$deadline_s" "$start_s" "$poll_s" "$call_s" "$reruns_max"; do
  case $setting in
    '' | *[!0-9]*) unavailable "a CI_VERIFY_* setting is not a whole number: '$setting'" ;;
  esac
done

now() { date +%s; }
started=$(now)
minutes() { printf '%dm%02ds' $(($1 / 60)) $(($1 % 60)); }

# Sleeps in the background, so a signal's trap runs at once rather than after the sleep.
pid=''
pause() {
  sleep "$1" &
  pid=$!
  wait "$pid"
  pid=''
}

# Runs a command with stdin from /dev/null, killed after call_s seconds (0: no bound).
dog=''
bounded() {
  if [ "$call_s" -eq 0 ]; then
    "$@" </dev/null
    return
  fi
  "$@" </dev/null &
  pid=$!
  (
    trap 'kill "$nap" 2>/dev/null; exit 0' TERM
    sleep "$call_s" &
    nap=$!
    wait "$nap" && kill -TERM "$pid" 2>/dev/null
  ) </dev/null >/dev/null 2>&1 &
  dog=$!
  wait "$pid"
  rc=$?
  pid=''
  kill "$dog" 2>/dev/null
  wait "$dog" 2>/dev/null
  dog=''
  return "$rc"
}

# owner/name from a github.com remote URL: git@github.com:o/n(.git), ssh://[user@]github.com[:port]/o/n,
# https://[user@]github.com/o/n. Anything else fails.
repo_of() {
  case $1 in
    git@github.com:*) path=${1#git@github.com:} ;;
    ssh://* | https://*)
      rest=${1#*://}
      host=${rest%%/*}
      path=${rest#*/}
      [ "$path" != "$rest" ] || return 1
      host=${host##*@}
      case $host in
        github.com | github.com:22 | github.com:443) ;;
        *) return 1 ;;
      esac
      ;;
    *) return 1 ;;
  esac
  path=${path%/}
  path=${path%.git}
  case $path in
    '' | /* | */ | */*/* | *[!A-Za-z0-9._/-]*) return 1 ;;
    */*) ;;
    *) return 1 ;;
  esac
  printf '%s\n' "$path"
}

sha=$(git rev-parse --verify HEAD 2>/dev/null) || unavailable "cannot read HEAD"
case $sha in
  *[!0-9a-f]* | '') unavailable "HEAD is not a commit id: $sha" ;;
esac
push_url=$(git remote get-url --push origin 2>/dev/null) || unavailable "origin has no push URL"
repo=$(repo_of "$push_url") || unavailable "origin's push URL does not name a github.com repository"
branch="qualify/factory-$sha"
ref="refs/heads/$branch"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/ci-verify.XXXXXX") || unavailable "cannot make a scratch directory"
pushed=0
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  [ -z "$pid" ] || kill "$pid" 2>/dev/null
  [ -z "$dog" ] || kill "$dog" 2>/dev/null
  pid=''
  dog=''
  if bounded git push --quiet origin --delete "$ref" >/dev/null 2>&1; then
    say "deleted $branch"
  elif [ "$pushed" = 1 ]; then
    say "could not delete $branch; delete it by hand"
  fi
  rm -rf "$tmp"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 75' HUP INT TERM

set -f
# shellcheck disable=SC2086 # the list is split on spaces, with globbing off
set -- $required
set +f
[ "$#" -gt 0 ] || unavailable "CI_VERIFY_REQUIRED names no check"

say "checking $sha on $repo; required: $*; gives up after $(minutes "$deadline_s")"

bounded gh api "repos/$repo" >/dev/null 2>"$tmp/err" || {
  cat "$tmp/err" >&2
  unavailable "GitHub is unreachable, or gh is not authenticated for $repo"
}

# Deletes the qualify branch, if it is there, and pushes it again, so each call starts a run.
push_fresh() {
  bounded git push --quiet origin --delete "$ref" >/dev/null 2>&1 || :
  attempt=1
  while :; do
    if bounded git push --quiet origin "$sha:$ref" >"$tmp/push" 2>&1; then
      pushed=1
      pushed_at=$(now)
      say "pushed $sha to $branch"
      return 0
    fi
    cat "$tmp/push" >&2
    [ "$attempt" -lt "$push_attempts" ] || return 1
    attempt=$((attempt + 1))
    pause 10
  done
}

# Starts a fresh run after the platform stopped one; only check runs above $1 count from now on.
fresh_run() {
  if [ "$reruns" -ge "$reruns_max" ]; then
    unavailable "the platform stopped the run, and $reruns_max fresh runs did not fix it"
  fi
  reruns=$((reruns + 1))
  floor=$1
  say "the platform stopped the run; starting a fresh one ($reruns of $reruns_max)"
  push_fresh || unavailable "cannot push $branch"
}

floor=0
reruns=0
poll_failures=0
reported=''
heartbeat=$started
while :; do
  ok=0
  listed=0
  if bounded gh api --paginate "repos/$repo/commits/$sha/check-runs?per_page=100&filter=all" \
    >"$tmp/runs" 2>"$tmp/err"; then
    listed=1
  elif grep -q 'No commit found for SHA.*(HTTP 422)' "$tmp/err"; then
    # GitHub has never seen this commit, so nothing has checked it: it is pushed next.
    printf '{"check_runs": []}\n' >"$tmp/runs"
    listed=1
  fi
  if [ "$listed" = 1 ] &&
    python3 -I -B "$evaluate" checks "$tmp/runs" "$floor" "$@" >"$tmp/eval" 2>>"$tmp/err"; then
    ok=1
    verdict=$(sed -n 1p "$tmp/eval")
    max_id=$(sed -n 2p "$tmp/eval")
    sed -n '3,$p' "$tmp/eval" >"$tmp/checks"
    if [ "$verdict" = fail ]; then
      # A runner failure is reported as a failed job; only its annotations tell it apart.
      real=0
      awk -F '\t' '$3 ~ /^failed:/ { print $2 }' "$tmp/checks" >"$tmp/failed"
      while read -r id; do
        if bounded gh api --paginate "repos/$repo/check-runs/$id/annotations?per_page=100" \
          >"$tmp/notes" 2>"$tmp/err" &&
          kind=$(python3 -I -B "$evaluate" annotations "$tmp/notes" 2>>"$tmp/err"); then
          [ "$kind" = infra ] || real=1
        else
          ok=0
          break
        fi
      done <"$tmp/failed"
      [ "$ok" = 0 ] || [ "$real" = 1 ] || verdict=infra
    fi
  fi

  elapsed=$(($(now) - started))
  if [ "$ok" = 1 ]; then
    poll_failures=0
    current=$(cat "$tmp/checks")
    if [ "$current" != "$reported" ] || [ $(($(now) - heartbeat)) -ge 300 ]; then
      while IFS="$(printf '\t')" read -r name _run state url; do
        say "$name: $state $url ($(minutes "$elapsed"))"
      done <"$tmp/checks"
      reported=$current
      heartbeat=$(now)
    fi
    case $verdict in
      pass)
        say "passed: $sha"
        exit 0
        ;;
      fail)
        say "failed: $sha"
        exit 1
        ;;
      infra) fresh_run "$max_id" ;;
      missing)
        if [ "$pushed" = 0 ]; then
          push_fresh || unavailable "cannot push $branch"
        elif [ $(($(now) - pushed_at)) -ge "$start_s" ]; then
          unavailable "a required check had not started $(minutes "$start_s") after the push"
        fi
        ;;
      pending) ;;
      *) unavailable "unexpected verdict '$verdict'" ;;
    esac
  else
    poll_failures=$((poll_failures + 1))
    cat "$tmp/err" >&2
    if [ "$poll_failures" -ge "$poll_failures_max" ]; then
      unavailable "GitHub did not answer $poll_failures polls in a row"
    fi
    say "GitHub did not answer (poll $poll_failures of $poll_failures_max); trying again"
  fi

  elapsed=$(($(now) - started))
  left=$((deadline_s - elapsed))
  [ "$left" -gt 0 ] || unavailable "no answer for $sha within $(minutes "$deadline_s")"
  if [ "$left" -lt "$poll_s" ]; then nap=$left; else nap=$poll_s; fi
  [ "$nap" -eq 0 ] || pause "$nap"
done
