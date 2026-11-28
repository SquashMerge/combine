# The herdr-core lane's safety checks and its one `hc` environment, as POSIX sh functions.
# tools/verify-herdr-core sources this file; crates/combined/tests/contract/lane_safety.rs
# holds the same rules in Rust and runs both on one table.
#
# Function definitions only: sourcing this file runs nothing and assigns nothing, so the tests
# can source it without touching a core. Even the constants are functions.
#
# Portability: lane_check_own_child compares `ps -o comm=` with the executable's path or its
# base name. macOS prints the full path, so it passes there. On Linux comm is cut to 15
# characters, the comparison fails, and nothing is signalled (it fails closed). macOS is the
# only target; do not loosen the comparison. If the lane ever runs off macOS, also note that a
# lane core sharing the test's process group makes check_kill_target (lane_safety.rs) refuse
# every crash: treat that as a stop-and-report, not as a check to relax.

lane_path() { printf '%s' /usr/bin:/bin:/usr/sbin:/sbin; }

# This machine's default core id, hard-coded. The lane refuses it and any id in
# COMBINE_DEFAULT_CORE_IDS (lane_check_not_default). The lane never asks a live `hc` for the
# default core's id: asked while no default core runs, hc would start one.
lane_default_core_id() { printf '%s' 4bcff4a58f4d97d505396e5a43a807e6; }

# lane_check_not_default ID [OTHERS]: ID is set and is neither the hard-coded default core id
# nor any id in OTHERS, a space-separated list of further default-core ids the caller supplied
# (COMBINE_DEFAULT_CORE_IDS, validated by the caller to letters, digits, `_` and `-`).
lane_check_not_default() {
  [ -n "$1" ] && [ "$1" != "$(lane_default_core_id)" ] || return 1
  for other in ${2:-}; do
    [ "$1" != "$other" ] || return 1
  done
  return 0
}

# lane_check_root ROOT: /tmp/chc.XXXXXX or its /private form, six letters and digits.
lane_check_root() {
  case $1 in
    /private/tmp/chc.??????) ;;
    /tmp/chc.??????) ;;
    *) return 1 ;;
  esac
  case ${1#*/chc.} in
    *[!A-Za-z0-9]*) return 1 ;;
  esac
  return 0
}

lane_root_suffix() {
  lane_check_root "$1" || return 1
  printf '%s' "${1#*/chc.}"
}

# lane_check_session SESSION ROOT: "c" plus the root's suffix.
lane_check_session() {
  lane_root_suffix "$2" >/dev/null || return 1
  [ "$1" = "c$(lane_root_suffix "$2")" ]
}

lane_expected_socket() { printf '%s/run/herdr-core/%s.sock' "$1" "$2"; }

# lane_check_socket_path SOCKET ROOT SESSION HOME
lane_check_socket_path() {
  lane_check_session "$3" "$2" || return 1
  case $1 in
    "$4"/.local/state/herdr-core | "$4"/.local/state/herdr-core/*) return 1 ;;
  esac
  [ "$1" = "$(lane_expected_socket "$2" "$3")" ] || return 1
  [ "${#1}" -le 103 ]
}

lane_check_core_id() { [ -n "$1" ] && [ -n "$2" ] && [ "$1" = "$2" ]; }

# lane_check_kind PATH socket|dir: the kind itself, never a symlink to it.
lane_check_kind() {
  case $2 in
    socket) [ -S "$1" ] && [ ! -L "$1" ] ;;
    dir) [ -d "$1" ] && [ ! -L "$1" ] ;;
    *) return 1 ;;
  esac
}

# lane_check_own_child PID EXE OBSERVED_PPID OBSERVED_COMM SELF: PID is a decimal above 1 whose
# parent is SELF and whose command is EXE or its base name.
lane_check_own_child() {
  case $1 in
    '' | *[!0-9]* | 0 | 1 | 0*) return 1 ;;
  esac
  [ -n "$2" ] && [ -n "$3" ] && [ -n "$5" ] || return 1
  [ "$3" = "$5" ] || return 1
  [ "$4" = "$2" ] || [ "$4" = "${2##*/}" ]
}

# Every `hc` the lane runs gets exactly this environment and nothing else.
lane_hc() {
  env -i PATH="$(lane_path)" HOME="${HOME:-}" USER="${USER:-}" LOGNAME="${LOGNAME:-}" \
    TMPDIR="${TMPDIR:-/tmp}" HC_SESSION="$LANE_SESSION" XDG_STATE_HOME="$LANE_ROOT/state" \
    XDG_CONFIG_HOME="$LANE_ROOT/config" XDG_RUNTIME_DIR="$LANE_ROOT/run" "$LANE_HC" "$@"
}

# The same, with the socket named, for status and stop.
lane_hc_socket() {
  env -i PATH="$(lane_path)" HOME="${HOME:-}" USER="${USER:-}" LOGNAME="${LOGNAME:-}" \
    TMPDIR="${TMPDIR:-/tmp}" HC_SESSION="$LANE_SESSION" XDG_STATE_HOME="$LANE_ROOT/state" \
    XDG_CONFIG_HOME="$LANE_ROOT/config" XDG_RUNTIME_DIR="$LANE_ROOT/run" \
    HC_SOCKET="$LANE_SOCKET" "$LANE_HC" "$@"
}

