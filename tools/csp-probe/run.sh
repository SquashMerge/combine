#!/bin/sh
# Builds the aarch64 Combine.app with the csp-probe feature, ad-hoc signs it, and runs the
# packaged-app CSP probe tests against it (SPEC.md M2-S5, docs/m2-s5-csp.md). Needs the GUI
# session. Run from the checkout root, whole, under heavy-medium.sh:
#   CARGO_TARGET_DIR=... heavy-medium.sh sh tools/csp-probe/run.sh
set -eu

: "${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR to the build directory of this checkout}"
export CARGO_TARGET_DIR

# A bundle built here is signed ad hoc only, and nothing is notarized.
unset APPLE_SIGNING_IDENTITY APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_ID APPLE_PASSWORD \
  APPLE_TEAM_ID APPLE_API_KEY APPLE_API_KEY_PATH APPLE_API_ISSUER \
  TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD

npm ci --prefix app/web --ignore-scripts </dev/null
npm run build --prefix app/web </dev/null

(
  cd app/src-tauri
  ../web/node_modules/.bin/tauri build --target aarch64-apple-darwin --features csp-probe \
    --config '{"identifier":"com.squashmerge.combine.cspprobe"}' </dev/null
)

app="$CARGO_TARGET_DIR/aarch64-apple-darwin/release/bundle/macos/Combine.app"
# An arm64 executable must carry at least an ad-hoc signature to run.
/usr/bin/codesign --force --deep --sign - "$app"

COMBINE_CSP_PROBE_APP="$app" /usr/bin/caffeinate -dimsu \
  cargo test --locked -p combine-app --test csp_probe -- --ignored --nocapture --test-threads=1
