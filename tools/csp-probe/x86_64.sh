#!/bin/sh
# Builds the shipped x86_64 Combine.app (no csp-probe feature, no config override) and checks
# its executable: it is x86_64 and holds no probe code. It does not launch the bundle; that is
# an S9 step on the Intel MBP (docs/m2-s5-csp.md). Run from the checkout root, whole, under
# heavy-medium.sh:
#   CARGO_TARGET_DIR=... heavy-medium.sh sh tools/csp-probe/x86_64.sh
set -eu

: "${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR to the build directory of this checkout}"
export CARGO_TARGET_DIR

unset APPLE_SIGNING_IDENTITY APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_ID APPLE_PASSWORD \
  APPLE_TEAM_ID APPLE_API_KEY APPLE_API_KEY_PATH APPLE_API_ISSUER \
  TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD

npm ci --prefix app/web --ignore-scripts </dev/null
npm run build --prefix app/web </dev/null

(
  cd app/src-tauri
  ../web/node_modules/.bin/tauri build --target x86_64-apple-darwin </dev/null
)

app="$CARGO_TARGET_DIR/x86_64-apple-darwin/release/bundle/macos/Combine.app"
exe=$(/usr/bin/plutil -extract CFBundleExecutable raw -o - "$app/Contents/Info.plist")
bin="$app/Contents/MacOS/$exe"

archs=$(/usr/bin/lipo -archs "$bin")
echo "lipo -archs: $archs"
if [ "$archs" != "x86_64" ]; then
  echo "x86_64.sh: expected x86_64, got $archs" >&2
  exit 1
fi

if grep -a -q -e COMBINE_CSP_PROBE_OUT -e COMBINE_CSP_PROBE_DATA "$bin"; then
  echo "x86_64.sh: probe code in the shipped bundle" >&2
  exit 1
fi

echo "bundle: $app"
echo "CFBundleExecutable: $exe"
echo "no probe markers"
