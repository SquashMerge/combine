# Vendored Pele

The headless Mermaid validator the loader adapter evaluates inside its JS engine. The files here are copied byte for byte from the npm tarball below; nothing is edited, reformatted or renamed except for the version in the bundle's file name.

- **Package:** npm `pele@1.13.0`, upstream and unpatched. MIT licensed. Source: https://github.com/obsidianmd/pele. Published with npm provenance.
- **Not this:** it is not Trellis's `pele-1.10.0-trellis.2.js`, which carries Trellis's patches.
- **Tarball:** https://registry.npmjs.org/pele/-/pele-1.13.0.tgz
  - integrity: `sha512-Ie8UKY9x00MsuR3jO6AzUwzK2Z6MIV3Tf4EDdITiuk8Uaz8K7g0R082WqsqNaSU8DdgkrRvZThzhit83mi/uiw==`
- **Dates:** published 2026-10-31; vendored 2026-11-24.

## Files

| File | From | Bytes | SHA-256 |
| --- | --- | --- | --- |
| `pele-1.13.0.global.js` | `package/dist/pele.global.js` | 733,670 | `0b0a46c57a5a6971670c09f5ec4066705bd78505f90fd11adf6260325efa0910` |
| `LICENSE` | `package/LICENSE` | 1,934 | `311bccc30de6f7b9cbfb64232bb0c911ccb2191abe316550813ae25b2c7925b9` |

**Not vendored:** `package/dist/pele.js` (the ESM build), SHA-256 `37bea5005aa6d2dde929032ced5d2958cf642667cbc1fe675f9dfacf4ba15a51`. The web renderer installs it from the same tarball, and its lockfile integrity must equal the one above.

## Mermaid version

The package README says, at line 87 of `package/README.md`: "Pele targets Mermaid 12.1.0 syntax, with its own layout and visual style." The bundle itself does not carry the claim (`grep -c '12\.1'` on it prints 0).

## Re-verifying

```sh
S=$(mktemp -d)
npm pack pele@1.13.0 --ignore-scripts --pack-destination "$S"
printf 'sha512-%s\n' "$(openssl dgst -sha512 -binary "$S/pele-1.13.0.tgz" | openssl base64 -A)"
mkdir "$S/pkg" && tar -xzf "$S/pele-1.13.0.tgz" -C "$S/pkg"
shasum -a 256 "$S/pkg/package/dist/pele.global.js" "$S/pkg/package/dist/pele.js" "$S/pkg/package/LICENSE"
shasum -a 256 pele-1.13.0.global.js LICENSE
```

The first checksum line must equal the integrity above, and the vendored files must hash as in the table.

## Updating

A new version gets a new file name, new constants in `pele.rs`, a new copy of this README, a SPEC update and a new pin in the web renderer's lockfile.
