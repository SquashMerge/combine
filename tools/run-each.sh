#!/bin/sh
# Runs commands one after another and stops at the first that fails. The factory's evidence
# entries use it to run several checks in a row, so a failure can't be hidden by a later pass.
#
#   sh tools/run-each.sh CMD [ARG...] [-- CMD [ARG...]]...
#
# The arguments are split into groups at each literal `--`, and each group runs as one command
# with its words passed as they are: no eval, no `sh -c`, no word splitting, so spaces, globs,
# `$`, quotes, `;` and empty strings reach the command unchanged. A group can't hold a `--`.
#
# Exit status: 0 when every group passed; otherwise the status of the first group that failed
# (127 when its command is missing), and the groups after it don't run. With no arguments, a
# leading or trailing `--`, or two in a row (an empty group), it prints a usage line and exits
# 2 before running anything. Each group is printed to stderr before it runs, and a failing
# group is printed again with its status.
set -u

usage() {
  echo "usage: sh tools/run-each.sh CMD [ARG...] [-- CMD [ARG...]]..." >&2
  exit 2
}

[ $# -gt 0 ] || usage
previous=--
for word in "$@"; do
  if [ "$word" = -- ] && [ "$previous" = -- ]; then usage; fi
  previous=$word
done
[ "$previous" != -- ] || usage

# The list ends in `--`, so every group is followed by one.
set -- "$@" --
while [ $# -gt 0 ]; do
  # Move the first group to the back, so the list reads: the groups still to run, then this one.
  size=0
  while [ "$1" != -- ]; do
    set -- "$@" "$1"
    shift
    size=$((size + 1))
  done
  shift
  later=$(($# - size))
  (
    shift "$later"
    printf 'run-each: +' >&2
    printf ' %s' "$@" >&2
    printf '\n' >&2
    "$@"
    status=$?
    if [ "$status" -ne 0 ]; then
      printf 'run-each: failed with status %s:' "$status" >&2
      printf ' %s' "$@" >&2
      printf '\n' >&2
    fi
    exit "$status"
  )
  status=$?
  [ "$status" -eq 0 ] || exit "$status"
  # Drop this group: bring the later ones to the front, then remove it from the back.
  while [ "$later" -gt 0 ]; do
    set -- "$@" "$1"
    shift
    later=$((later - 1))
  done
  shift "$size"
done
