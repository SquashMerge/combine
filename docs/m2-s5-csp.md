# M2-S5: CSP probe of the packaged app

Evidence for AC13 (SPEC.md, M2-S5; issue #48). The packaged app, built with the `csp-probe`
feature, draws a Mermaid panel and a Markdown panel served by an isolated in-process daemon,
and its webview reports no Content-Security-Policy violation. A second run plants a violation
to show the probe can see one. The shipped x86_64 bundle is built and checked for probe code.

## Host

- `hostname -s`: Jasons-Mac-Mini
- `sw_vers -productVersion`: 27.2
- Architecture: arm64 (`uname -m`), no Rosetta (`sysctl -n sysctl.proc_translated` is 0)
- Session: logged-in GUI session

## Build

- Build SHA (`git rev-parse HEAD`, clean tree): `6c548adb876028d5b958263700a66aa39d35d38e`
- Remediation (`dangerousDisableAssetCspModification`): not applied; `tauri.conf.json` is unchanged.
- CSP string from `app/src-tauri/tauri.conf.json`:

```
default-src 'self'; connect-src ipc: http://ipc.localhost; style-src 'self' 'unsafe-inline'; img-src 'self' data:
```

## Commands

aarch64 probe bundle and probe run:

```sh
CARGO_TARGET_DIR=/Users/jcarreira/projects/squashmerge/combine-targets/48-e-csp-probe \
  /Users/jcarreira/projects/squashmerge/trellis-evidence/heavy-lock/heavy-medium.sh sh tools/csp-probe/run.sh
```

x86_64 shipped bundle and marker check:

```sh
CARGO_TARGET_DIR=/Users/jcarreira/projects/squashmerge/combine-targets/48-e-csp-probe \
  /Users/jcarreira/projects/squashmerge/trellis-evidence/heavy-lock/heavy-medium.sh sh tools/csp-probe/x86_64.sh
```

## Probe run (aarch64)

Launch mode used: `exec` (the app's executable started directly with a cleared environment).
The `open` fallback was not needed: there was no window-server error. Both tests passed
(`2 passed`), and the app exited with status 0 each time.

### result.json (clean run, verbatim)

```json
{
  "version": 1,
  "renders": [
    {
      "space": "/tmp/c48-24526-0/probe/work",
      "panel": "probe-mermaid",
      "revision": "1",
      "status": "rendered",
      "error": null
    },
    {
      "space": "/tmp/c48-24526-0/probe/work",
      "panel": "probe-markdown",
      "revision": "2",
      "status": "rendered",
      "error": null
    }
  ],
  "violations": [],
  "violationCount": 0,
  "timedOut": false
}
```

### Daemon render status

Polled every 50 ms with `PanelHandle::render_status` while the app ran; the first poll that
showed each panel rendered at its current revision:

```
render status: probe-mermaid: revision 1 Rendered after 12.903214041s
render status: probe-markdown: revision 2 Rendered after 12.995791458s
```

## Planted violation run

`COMBINE_CSP_PROBE_PLANT=1` adds an `<img>` for `https://example.invalid/planted.png` once the
page is ready. Its render status lines:

```
render status: probe-mermaid: revision 1 Rendered after 1.845890666s
render status: probe-markdown: revision 2 Rendered after 1.845979208s
```

The violation excerpt from its result.json (`violationCount` 1):

```json
"violations": [
    {
      "directive": "img-src",
      "effectiveDirective": "img-src",
      "blockedUri": "https://example.invalid/planted.png",
      "sourceFile": "user-script",
      "line": 15,
      "column": 6,
      "sample": "",
      "disposition": "enforce"
    }
  ]
```

Matched in the test as:

```
planted violation: Some(Object {"blockedUri": String("https://example.invalid/planted.png"), "column": Number(6), "directive": String("img-src"), "disposition": String("enforce"), "effectiveDirective": String("img-src"), "line": Number(15), "sample": String(""), "sourceFile": String("user-script")})
```

## x86_64 shipped bundle

Built with `tauri build --target x86_64-apple-darwin`, with no feature and no config override.

```
lipo -archs: x86_64
bundle: /Users/jcarreira/projects/squashmerge/combine-targets/48-e-csp-probe/x86_64-apple-darwin/release/bundle/macos/Combine.app
CFBundleExecutable: combine-app
no probe markers
```

- Path: `/Users/jcarreira/projects/squashmerge/combine-targets/48-e-csp-probe/x86_64-apple-darwin/release/bundle/macos/Combine.app`
- `CFBundleExecutable`: `combine-app`
- `lipo -archs`: `x86_64`
- Marker check (`grep -a` for `COMBINE_CSP_PROBE_OUT` and `COMBINE_CSP_PROBE_DATA`): 0 hits

## Notes

A plain `cargo build --release` would be a dev-mode (devUrl) binary, so the check builds the bundle with `tauri build`.

Launching the x86_64 bundle on the Intel MBP is an S9 acceptance step.

`app/src-tauri/Cargo.toml` spells the `tauri` and `tauri-build` dependencies as the tauri CLI writes them (`{ version = "=...", features = [] }`), so `tauri build` leaves the file unchanged.
