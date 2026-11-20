# Source this: isolated herdr-core for M0. Never touches the user's default core.
export M0=${M0:-/tmp/cm0}
export M0_SOCK=$M0/run/herdr-core/m0.sock
export CARGO_TARGET_DIR=/Users/jcarreira/projects/squashmerge/combine-targets/m0-spike
export HEAVY=/Users/jcarreira/projects/squashmerge/trellis-evidence/heavy-lock/heavy.sh
DEFAULT_CORE_ID=4bcff4a58f4d97d505396e5a43a807e6
mkdir -p $M0/state $M0/config $M0/run; chmod 700 $M0 $M0/run
hcm() { env -u HC_TERMINAL_ID -u HC_TERMINAL_SOCKET -u HC_SCOPE -u HC_SOCKET HC_SESSION=m0 \
  XDG_STATE_HOME=$M0/state XDG_CONFIG_HOME=$M0/config XDG_RUNTIME_DIR=$M0/run hc "$@"; }
# Refuse to continue if the isolated session somehow resolves to the default core.
hcm_safe() {
  local id; id=$(hcm server status --json | python3 -I -c 'import json,sys;print(json.load(sys.stdin)["core"]["id"])')
  if [ "$id" = "$DEFAULT_CORE_ID" ]; then echo "REFUSING: isolated hc resolves to the default core" >&2; return 1; fi
  echo "isolated core id $id"
}
