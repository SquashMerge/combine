#!/bin/sh
# The feature factory's repository `verify` (.factory.json): runs ./tools/verify on GitHub
# Actions for the commit checked out here, instead of locally under the shared heavy-run lease.
#
#   sh tools/ci_verify.sh
#
# It pushes HEAD to qualify/factory-<sha> on origin's push URL, which starts
# .github/workflows/verify.yml, then waits on the check runs GitHub records for that exact sha
# (never "the latest run on the branch"), taking the newest run of each required check. Only
# check runs from a workflow run that a push of this sha started count (a qualify branch, or
# main): those checked out this sha. A pull_request run is attached to the same sha but checked
# out a merge commit, so it never counts. The qualify branch is deleted on every exit; that
# doesn't cancel a run still going.
#
# Exit status:
#   0   every required check is present and its newest run passed
#   1   the newest run of a required check failed
#   75  no answer (.factory.json verify_unavailable_exit): GitHub unreachable or gh not
#       authenticated, the push failed, origin's push URL names no github.com repository, no
#       required check started within CI_VERIFY_START_S of the push, the platform stopped the
#       run (conclusion cancelled, startup_failure, stale or action_required) more times than
#       CI_VERIFY_MAX_RERUNS fresh runs could fix, or the time budget ran out.
# The factory pauses on 75 and picks the run up again later; there is no local fallback.
#
# A required check that already has a usable push run on this sha (passed, or still going) is
# waited on without a new push, so a re-run of this script reuses it. One the platform stopped
# gets a fresh run: the branch is deleted and pushed again, and only check runs newer than the
# stopped one count.
#
# Time: CI_VERIFY_DEADLINE_S is one hard budget for everything, cleanup included, below the
# factory's verify_timeout_ms (5400000, 90 minutes). No new work (a poll, a push, a sleep)
# starts in the last CI_VERIFY_RESERVE_S, which is kept for deleting the branch, and no git or
# gh call is allowed to run past the time left for it: it gets TERM, then KILL 5 s later.
#
# Settings (environment), all whole seconds:
#   CI_VERIFY_REQUIRED          required check names, space-separated (default: verify)
#   CI_VERIFY_DEADLINE_S        the hard budget (default: 4800, 80 minutes)
#   CI_VERIFY_RESERVE_S         the end of the budget kept for cleanup (default: 300)
#   CI_VERIFY_START_S           how long after a push a required check may stay missing
#                               (default: 600)
#   CI_VERIFY_POLL_S            between polls (default: 30)
#   CI_VERIFY_CALL_TIMEOUT_S    bound on each git or gh call, below the time left; 0 for only
#                               the time left (default: 120)
#   CI_VERIFY_MAX_RERUNS        fresh runs after the platform stops one (default: 2)
#
# gh uses its ambient authentication (the factory driver exports GH_TOKEN); nothing here reads,
# prints or passes a token. The GitHub repository is named from origin's push URL, because in a
# factory sandbox origin fetches from a local path and gh can't infer it. Neither git's nor gh's
# own error output is printed, since it can hold a credentialed URL: only a git exit status, and
# gh's first lines with any URL userinfo and token-like words removed.
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
reserve_s=${CI_VERIFY_RESERVE_S:-300}
start_s=${CI_VERIFY_START_S:-600}
poll_s=${CI_VERIFY_POLL_S:-30}
call_s=${CI_VERIFY_CALL_TIMEOUT_S:-120}
reruns_max=${CI_VERIFY_MAX_RERUNS:-2}
poll_failures_max=5
push_attempts=3

for setting in "$deadline_s" "$reserve_s" "$start_s" "$poll_s" "$call_s" "$reruns_max"; do
  case $setting in
    '' | *[!0-9]*) unavailable "a CI_VERIFY_* setting is not a whole number: '$setting'" ;;
  esac
done

now() { date +%s; }
started=$(now)
hard_end=$((started + deadline_s))
work_end=$((hard_end - reserve_s))
minutes() { printf '%dm%02ds' $(($1 / 60)) $(($1 % 60)); }
out_of_time() {
  unavailable "no answer for $sha within $(minutes $((work_end - started))) (budget $(minutes "$deadline_s"), cleanup included)"
}
in_budget() { [ "$(now)" -lt "$work_end" ]; }

# gh's error output, at most three lines, with URL userinfo and token-like words removed.
show_gh_error() {
  sed -e 's#://[^/[:space:]]*@#://#g' \
    -e 's/gh[opsuar]_[A-Za-z0-9_]*/[token]/g' -e 's/github_pat_[A-Za-z0-9_]*/[token]/g' "$1" |
    head -n 3 | sed 's/^/  gh: /' >&2
}

# Sleeps in the background, so a signal's trap runs at once rather than after the sleep; never
# past the end of the working budget.
pid=''
pause() {
  nap=$1
  left=$((work_end - $(now)))
  [ "$left" -gt "$nap" ] || nap=$left
  [ "$nap" -gt 0 ] || return 0
  sleep "$nap" &
  pid=$!
  wait "$pid"
  pid=''
}

# bounded END CMD...: runs CMD with stdin from /dev/null, sent TERM once it would run past the
# epoch second END (or after call_s seconds if that is sooner) and KILL kill_grace_s later, so
# it is gone by END even if it ignores TERM. Returns 124 without running it when no time is left.
# CMD sees its bound as CI_VERIFY_LIMIT_S (the tests' stand-ins use it).
kill_grace_s=5
dog=''

# watchdog PID SECONDS SIGNAL [SECONDS SIGNAL]: in the background, sends PID the first signal after
# the first wait, then the second after the second. Killing the watchdog stops it, sleep included.
watchdog() {
  target=$1
  (
    trap 'kill "$snooze" 2>/dev/null; exit 0' TERM
    /bin/sleep "$2" &
    snooze=$!
    wait "$snooze" || exit 0
    kill "-$3" "$target" 2>/dev/null || exit 0
    [ "$#" -ge 5 ] || exit 0
    /bin/sleep "$4" &
    snooze=$!
    wait "$snooze" && kill "-$5" "$target" 2>/dev/null
  ) </dev/null >/dev/null 2>&1 &
}

bounded() {
  limit=$(($1 - $(now) - kill_grace_s))
  shift
  [ "$limit" -gt 0 ] || return 124
  if [ "$call_s" -gt 0 ] && [ "$call_s" -lt "$limit" ]; then limit=$call_s; fi
  CI_VERIFY_LIMIT_S=$limit "$@" </dev/null &
  pid=$!
  watchdog "$pid" "$limit" TERM "$kill_grace_s" KILL
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
  *[!0-9a-f]* | '') unavailable "HEAD is not a commit id" ;;
esac
push_url=$(git remote get-url --push origin 2>/dev/null) || unavailable "origin has no push URL"
repo=$(repo_of "$push_url") || unavailable "origin's push URL does not name a github.com repository"
branch="qualify/factory-$sha"
ref="refs/heads/$branch"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/ci-verify.XXXXXX") || unavailable "cannot make a scratch directory"
pushed=0
# Stops a child still running when the script is cut short (a git or gh call, or a sleep):
# TERM, KILL kill_grace_s later if it ignores TERM, and waits for it, so none outlives the
# script. Then stops that call's watchdog.
stop_child() {
  if [ -n "$pid" ]; then
    kill -TERM "$pid" 2>/dev/null
    watchdog "$pid" "$kill_grace_s" KILL
    reaper=$!
    wait "$pid" 2>/dev/null
    kill "$reaper" 2>/dev/null
    wait "$reaper" 2>/dev/null
  fi
  if [ -n "$dog" ]; then
    kill "$dog" 2>/dev/null
    wait "$dog" 2>/dev/null
  fi
  pid=''
  dog=''
}

cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  stop_child
  if bounded "$hard_end" git push --quiet origin --delete "$ref" >/dev/null 2>&1; then
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

say "checking $sha on $repo; required: $*; gives up by $(minutes "$deadline_s"), cleanup included"

in_budget || out_of_time
bounded "$work_end" gh api "repos/$repo" >/dev/null 2>"$tmp/err" || {
  show_gh_error "$tmp/err"
  unavailable "GitHub is unreachable, or gh is not authenticated for $repo"
}

# Deletes the qualify branch, if it is there, and pushes it again, so each call starts a run.
push_fresh() {
  in_budget || out_of_time
  bounded "$work_end" git push --quiet origin --delete "$ref" >/dev/null 2>&1 || :
  attempt=1
  while :; do
    in_budget || out_of_time
    bounded "$work_end" git push --quiet origin "$sha:$ref" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then
      pushed=1
      pushed_at=$(now)
      say "pushed $sha to $branch"
      return 0
    fi
    say "git push exited $rc (attempt $attempt of $push_attempts)"
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

# Lists this sha's check runs and push workflow runs; 0 when both answered.
list_runs() {
  if ! bounded "$work_end" gh api --paginate \
    "repos/$repo/commits/$sha/check-runs?per_page=100&filter=all" >"$tmp/runs" 2>"$tmp/err"; then
    grep -q 'No commit found for SHA.*(HTTP 422)' "$tmp/err" || return 1
    # GitHub has never seen this commit, so nothing has checked it: it is pushed next.
    printf '{"check_runs": []}\n' >"$tmp/runs"
  fi
  bounded "$work_end" gh api --paginate \
    "repos/$repo/actions/runs?head_sha=$sha&event=push&per_page=100" >"$tmp/workflows" 2>"$tmp/err"
}

floor=0
reruns=0
poll_failures=0
reported=''
heartbeat=$started
while :; do
  in_budget || out_of_time
  ok=0
  if list_runs &&
    python3 -I -B "$evaluate" "$tmp/runs" "$tmp/workflows" "$sha" "$floor" "$@" >"$tmp/eval" 2>"$tmp/err"; then
    ok=1
  fi

  if [ "$ok" = 1 ]; then
    poll_failures=0
    verdict=$(sed -n 1p "$tmp/eval")
    max_id=$(sed -n 2p "$tmp/eval")
    sed -n '3,$p' "$tmp/eval" >"$tmp/checks"
    current=$(cat "$tmp/checks")
    elapsed=$(($(now) - started))
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
    show_gh_error "$tmp/err"
    if [ "$poll_failures" -ge "$poll_failures_max" ]; then
      unavailable "GitHub did not answer $poll_failures polls in a row"
    fi
    say "GitHub did not answer (poll $poll_failures of $poll_failures_max); trying again"
  fi

  in_budget || out_of_time
  pause "$poll_s"
done
