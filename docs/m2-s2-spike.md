# M2-S2 spike: headless Mermaid validation

Run #45, 2026-11-24, base 6a074ce. Machine Mac17,16 (Apple M5 Pro), macOS 27.2, rustc 1.98.1 (48a229cea 2026-09-01), aarch64-apple-darwin; release build, measurements under `heavy.sh` (the large tier).

## Decisions

| Decision | Result | Evidence |
| --- | --- | --- |
| Engine (D2) | rquickjs 0.14.0, `default-features = false`, `features = ["std"]`, on quickjs-ng 0.16.2 | Pele evaluates in strict global mode with no shims (P2, P3); a release build needs no bindgen and takes 15.7 s (P1) |
| Validation mode (D4) | Full `render()`: it is synchronous and works headless, so `SUCCESS` is `Validated`; `parse()` is not needed | P4: `render(class.mmd)` returns an object (`svg.length` 4,158, 300 x 330, `type` `class`, `mermaidType` `classDiagram`), not a Promise |
| Mermaid syntax (D3) | 12.1.0, so SPEC stays (G7) | The package README, line 87: "Pele targets Mermaid 12.1.0 syntax"; the bundle doesn't carry the claim (`grep -c '12\.1'` prints 0) |
| Pele artifact (D3) | Upstream npm `pele@1.13.0`, published 2026-10-31 (24 days old when pinned), vendored byte-identically | Integrity and all three SHA-256 values match the brief (see "Pele artifact") |
| The 5 s wait | Met: the 200 KB warm p95 is 139.6 ms | P7: p50 138.2 ms, p95 139.6 ms |
| Memory | Met: R200 is 19 MiB, under the 64 MiB cap | P9: the smallest cap at which the 200 KB diagram answers `SUCCESS` is 19 MiB |
| "Loaded once" (R9) | The bundle is evaluated once per runtime; `Module::load` is `unsafe`, so no bytecode is loaded | P13; rquickjs-core `value/module.rs:339` is `pub unsafe fn load` |
| Fallback | None | G3 to G5 are go |
| Test inputs | `slow_input()` is `fan_out(226)`; `memory_input()` is `fan_out(226)`; W = 9 MiB, h = 1 MiB. Both picks need the amendments A1 and A2 below | "Test inputs" |
| Anchors needed | `broken-unknown-type` and `broken-front-matter`: Pele gives them no line (0) and no column (0). The other three get their position from Pele | P5; "Headless render and positions" |

**Amendments for s2-3-adapter** (the spike found them; the brief's text is wrong or incomplete on these points):
- **A1, a thrown `null` is an out-of-memory.** When QuickJS can't allocate even the error object, the JS `catch` sees the value `null` (`typeof` is `"object"`; `e === null`), not an `InternalError`. It happens at most caps (10 MiB and 12 MiB for the 200 KB diagram, 70 and 100 MiB for `fan_out(226)`), and the engine heap read after the call is only about 4 MB because the failed call has been unwound, so `heap_near_limit` is false. With the brief's `classify` rule 2 as written, the check at W + h answers `NoVerdict("null")`, not `MemoryLimit` (run 1 below). Rule 2 must also match a thrown value that is `null` (not an object, with no Pele `code`). Pele never throws `null` itself, and the source is a string argument, so the match is safe. With that rule, the probe confirms `MemoryLimit` at W + h.
- **A2, `fan_out(226)` as the time input dies of memory at the default caps.** With `time_limit` at 10 s and a 64 MiB heap, it ends in `MemoryLimit` after about 115 ms (the thrown `null` of A1), not in `Success`. It is `TimeLimit` only when the time cap is well below that, as at the 10 ms of P8 (interrupted after 10.6 ms). The rule of the brief (the first candidate with t_slow_min >= 80 ms and >= 80 x max(t_class, t_first)) picks it. `class_diagram_of_bytes(204_800)` also meets the rule (t_slow_min 138.4 ms) and runs to `Success` in pure compute, so it is the more robust slow input if s2-3's time test needs a verdict that doesn't depend on the memory cap. The driver can choose at checkpoint A; the numbers support both.

## Gates

| Gate | Go/no-go | Evidence |
| --- | --- | --- |
| G1 artifact | go | The integrity, and the sizes and SHA-256 of `pele.global.js`, `pele.js` and `LICENSE`, equal the brief's values; the vendored copies re-hash identically |
| G2 tooling | go | `check.py` takes 0.6 s and reports `arch-guard: clean`; `trellis rules check` exits 0 with the same counts before and after (unknown 290, unassigned files 2); see "Tooling" |
| G3 engine and mode | go | Pele evaluates under rquickjs in strict global mode; `render()` returns headless, synchronously |
| G4 thresholds | go | The 200 KB warm p95 is 139.6 ms (limit 5 s); R200 is 19 MiB (limit 64 MiB) |
| G5 caps | go | The busy loop stops at its 1 s deadline (overshoot under 0.1 ms); the allocation loop answers `out of memory`; 20 cycles finish with no abort. A running regex is interrupted: P8 found no watchdog case, so no watchdog issue is filed |
| G6 positions | go | `broken-syntax`, `broken-unclosed-block` and `broken-relation` get a line and a column from Pele; `broken-unknown-type` and `broken-front-matter` get theirs from `pele::anchor`. Variant A of every category fails with a position, so no variant B is committed |
| G7 Mermaid version | go | The README says 12.1.0; SPEC `:656` and `:1038` stay |
| G8 bytecode | go (recorded) | `Module::load` is `unsafe`; the bundle is evaluated once per runtime |
| G9 test inputs | go, with A1 and A2 | `slow_input()` and `memory_input()` are picked by the brief's rules; W = 9 MiB and h = 1 MiB (at most 16); `memory_input()` answers `MemoryLimit` at W + h only with amendment A1 |

## Pele artifact

- **Version:** `pele@1.13.0`; licence MIT; repository `git+https://github.com/obsidianmd/pele.git`.
- **Published:** 2026-10-31T00:24:29.976Z, 24 days before this run. The registry also lists 1.10.0 (2026-10-24), 1.10.1 and 1.11.0 (2026-10-26), 1.12.0 (2026-10-26) and 1.13.0; this is the newest release at least two weeks old.
- **Tarball:** https://registry.npmjs.org/pele/-/pele-1.13.0.tgz, fetched with `npm pack pele@1.13.0 --ignore-scripts`.
- **Integrity** (`npm view` and recomputed from the tarball with `openssl dgst -sha512 -binary | openssl base64 -A`, both equal): `sha512-Ie8UKY9x00MsuR3jO6AzUwzK2Z6MIV3Tf4EDdITiuk8Uaz8K7g0R082WqsqNaSU8DdgkrRvZThzhit83mi/uiw==`.
- **Recomputed SHA-256:**
  - `dist/pele.global.js`, 733,670 bytes: `0b0a46c57a5a6971670c09f5ec4066705bd78505f90fd11adf6260325efa0910`;
  - `dist/pele.js`, not vendored: `37bea5005aa6d2dde929032ced5d2958cf642667cbc1fe675f9dfacf4ba15a51`;
  - `LICENSE`, 1,934 bytes: `311bccc30de6f7b9cbfb64232bb0c911ccb2191abe316550813ae25b2c7925b9`.
- **Vendored with `cp`** to `crates/combined/src/adapters/loader/vendor/pele-1.13.0.global.js` and `.../LICENSE`; both re-hashed in place to the same size and SHA-256.
- **Mermaid:** README line 3 "Pele renders Mermaid diagrams to SVG", line 5 "Pele supports every diagram type built into Mermaid 12.1.0", line 87 "Pele targets Mermaid 12.1.0 syntax, with its own layout and visual style." `grep -c '12\.1'` on the bundle: 0.
- **Exports** (`Object.keys(pele)`): `PeleError`, `detectType`, `enableZoom`, `forgetTextWidths`, `mount`, `parse`, `register`, `registered`, `render`, `supports`. `typeof` of `render`, `parse` and `PeleError` is `function`; `pele.metricsMeasurer` is `undefined`, so no `measurer` is passed.
- **Option names and defaults,** quoted from the bundle:
  - `var C_=5e4,N_=4e6,I_=5e3;` are the defaults: `limit` 50,000, `outputLimit` 4,000,000, `maxEdges` 5,000;
  - `function Wm(e,t=C_,n=I_,r){if(e.length>t)throw new Qe(\`Diagram source is longer than the limit of ${t} characters.\`,"limit")` takes `limit` and `maxEdges` (from `t.limit`, `t.maxEdges`); `e.length` counts UTF-16 units;
  - `m=t.outputLimit??N_;if(d.svg.length>m)throw new Qe(\`Diagram output is longer than the limit of ${m} characters.\`,"limit",...)`;
  - the edge limit: `throw new Qe(\`Edge limit exceeded. The limit is ${l}.\`,"limit",...)`.
- **Values Combine passes:** `limit` 204,800 (the source-size limit, in bytes; Pele counts no more units than the source has bytes); `maxEdges` 51,200 (204,800 / 4, the cheapest edge spelling); `outputLimit` 4,000,000, Pele's own default, passed explicitly so the web renderer passes the same. The largest output seen, for the 200 KB diagram, is 1,826,043 characters, under 4,000,000, so `PELE_OUTPUT_LIMIT` stays 4,000,000 (it would be 16,000,000 at 4,000,000 or more).
- **Missing globals** (`typeof` in QuickJS 0.16.2 with `Context::full`): `console`, `Intl`, `TextEncoder`, `setTimeout`, `structuredClone`, `document` and `OffscreenCanvas` are `undefined`; `performance` (object), `queueMicrotask` and `WeakRef` (functions) exist. The bundle has 0 occurrences of `console.`, `Intl`, `TextEncoder`, `setTimeout`, `queueMicrotask` and `structuredClone`, and 1 of `performance`, 14 of `document`, 2 of `OffscreenCanvas` and 1 of `WeakRef`. `std`, `os`, `require`, `fetch`, `XMLHttpRequest`, `process` and `Deno` are all `undefined` (AC10).
- **Shims installed: none.** Pele loads and renders without a `console` or a `performance.now` shim. (The brief expected a no-op `console` and `performance.now` "only as P3 requires"; P3 doesn't require either.)

## Engine

- **rquickjs 0.14.0** with `["std"]` only, on the quickjs-ng 0.16.2 that rquickjs-sys 0.14.0 bundles (`QJS_VERSION` 0.16.2 in its `quickjs.h`); `bindgen` is off.
- **EvalOptions for the bundle:** `global = true`, `strict = true`, everything else default. Strict worked at the first try, so there was no retry with `strict = false`. The adapter uses the same options.
- **APIs used:** `Runtime::new`, `set_memory_limit`, `set_max_stack_size`, `set_interrupt_handler`, `Context::full`, `Ctx::eval_with_options`, `Ctx::catch`, `Runtime::run_gc`, `Runtime::memory_usage` (`malloc_size`, `memory_used_size`).
- **`Module::load` is `unsafe`,** so R9 means the bundle is evaluated once per runtime.
- **Thrown values** are read as `engine.rs` will read them (M11): for an Error object, `Exception::message()` and the `name` property; for any other object, its `message` property; for anything else, the `String` coercion; `code`, `line`, `column` and `snippet` are property reads.
- **Signals:**
  - interrupt: an `InternalError` with message `interrupted`, and the flag of the handler is set;
  - out of memory: an `InternalError` with message `out of memory`, or, when even the error object can't be allocated, the value `null` (amendment A1). Pele doesn't wrap either: a `PeleError` has a `code`, and neither of these has one;
  - stack overflow: a `RangeError` with message `Maximum call stack size exceeded`, also with no Pele code.
- **Regex interruptibility (P8):** `/(a+)+$/.test('a'.repeat(25)+'!')` under a 200 ms deadline stops after 200.1 ms with `interrupted`; run to the end under a 60 s cap it takes 642 ms. A running regex is interrupted. There is no Known limit and no watchdog issue.
- **20 cap-and-rebuild cycles (P11):** alternating the busy loop (1 s cap) and the allocation loop, each followed by a rebuild and a `class.mmd` render: no abort, and all 20 renders answer `Success`. The busy loops end `Time` at 1,000 ms; the allocation loops end `Mem` in 16 to 18 ms. Process RSS: 225,744 KiB at the start, 269,648 KiB at the end (it includes the earlier phases of the same process, up to a 512 MiB heap).
- **Threads and stacks:** the probe runs everything on an 8 MiB thread, with a 1 MiB JS stack; `f(1e6)` ends in the `RangeError` above and the process survives.

## Headless render and positions

- **`render(class.mmd)`** returns an object (not a Promise): keys `type`, `mermaidType`, `svg`, `width`, `height`, `links`; `svg.length` 4,158; `width` 300; `height` 330; `type` `class`; `mermaidType` `classDiagram`. Options `{}` give the same result.
- **`parse(class.mmd)`** returns an object (the model, not a Promise) with keys `type`, `lineType`, `relationType`, `direction`, `classes`, `relations`, `notes`, `namespaces`, `interfaces`, `styleClasses`, `title`, `accTitle`, `accDescr`, `namespaceStack`, `withCssClass`, `mermaidType`; `type` `class`, `mermaidType` `classDiagram`.
- **Error shape:** a `PeleError` with own properties `message`, `code`, `type`, `mermaidType`, `line`, `column`, `snippet` and `name` (`stack` is not an own property in QuickJS). `render` and `parse` throw the same error for every fixture. The default position is `line` 0, `column` 0, `snippet` `""`.
- **Position base:** 1-based for both. `@@@` in `broken-syntax` sits at line 6, column 3, and Pele says line 6, column 3.
- **Column unit:** UTF-16 code units. `  Animal <|-x- Dog` gives column 12; with `"é"` inserted the column is 16 (+4: one unit for `é`); with `"😀"` inserted it is 17 (+5: two units for the emoji; characters would give 16, bytes 19). Leading `éé` gives column 8.
- **Fixture table.** "Anchor?" says whether the position comes from `pele::anchor`. The anchored rows follow the corrected rule: `Rw` decides front matter (N3); whitespace is JS `\s`, which is `char::is_whitespace` plus U+FEFF minus U+0085 (N4); the column is 1 + the leading whitespace characters, not counting U+FEFF; the snippet is the line with trailing whitespace removed and U+FEFF dropped, keeping leading whitespace. The Pele snippet and message contain a caret line, shown with `\n`.

| Fixture | Variant | Pele name / code | Pele line / column | Anchor? | Expected line | Column | Snippet | Message (first 80 characters) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `broken-syntax` | A | `PeleError` / `syntax` | 6 / 3 | no | 6 | 3 | `  @@@ not a statement\n--^` | `Lexical error on line 6. Unrecognized text.\n  @@@ not a statement\n--^` |
| `broken-unknown-type` | A | `PeleError` / `unsupported-diagram` | 0 / 0 | yes, rule 2 (unsupported diagram, no front matter, first non-blank non-comment line is line 1) | 1 | 1 | `notADiagram` | `No diagram type detected for the given text.` |
| `broken-unclosed-block` | A | `PeleError` / `syntax` | 5 / 18 | no | 5 | 18 | `  Animal <|-- Dog\n-----------------^` | `Parse error on line 5:\n  Animal <\|-- Dog\n-----------------^\nExpecting 'STRUCT_ST` |
| `broken-relation` | A | `PeleError` / `syntax` | 4 / 12 | no | 4 | 12 | `  Animal <\|-x- Dog\n-----------^` | `Parse error on line 4:\n  Animal <\|-x- Dog\n-----------^\nExpecting 'AGGREGATION', ` |
| `broken-front-matter` | A | `PeleError` / `syntax` | 0 / 0 | yes, rule 1 (front matter closed at line 3, and the message holds "front matter" and "yaml") | 1 | 1 | `---` | `Front matter is not valid YAML. YAML: unexpected end of the stream within a flow` |

Variants not committed, recorded for completeness:
- `broken-relation` variant B (`  Animal <|-- ` as the last line): `syntax`, line 4, column 14, snippet `  Animal <|-- \n-------------^`.
- `broken-front-matter` variant B (never closed): `unsupported-diagram` "No diagram type detected for the given text.", line 0, column 0; the anchor's rule 1 (front matter unclosed) gives line 1, column 1, snippet `---`.

**Which categories needed the anchor:** unknown diagram type, and front matter (invalid YAML in closed front matter; an unclosed opener too). Pele gives no position for either.

**Pele's front-matter messages:**
- closed, invalid YAML: `Front matter is not valid YAML. YAML: unexpected end of the stream within a flow collection`, with code `syntax` (it contains both "front matter" and "yaml", so `FRONT_MATTER_WORDS` matches);
- not recognised as front matter (unclosed, or any of the cases under "Differences" below): `No diagram type detected for the given text.`, code `unsupported-diagram`, with no mention of front matter. Rule 1 therefore has to find an unclosed front matter by the structure, which is what the brief says.

**`Rw`, Pele's front-matter detector,** quoted from the bundle (`function Rw`), with `au(c)` = `c !== undefined && /\s/.test(c)` and `Ja(c)` = `c === "\n" || c === "\r"`:

```js
function Rw(e){let t=0;for(;t<e.length&&au(e[t])&&!Ja(e[t]);)t++;let n=e.slice(0,t);if(!e.startsWith("---",t))return;let r=[];for(let h=t+3;h<e.length&&au(e[h]);h++)Ja(e[h])&&r.push(h);if(r.length===0)return;let i=n+"---",s=h=>{if(!Ja(e[h])||!e.startsWith(i,h+1))return-1;let d=-1;for(let m=h+1+i.length;m<e.length&&au(e[m]);m++)Ja(e[m])&&(d=m);return d},a=`
`+i,o=-1;for(let h=e.lastIndexOf(a);h>r[0];h=e.lastIndexOf(a,h-1))if(s(h)!==-1){o=h;break}if(o===-1)return;...}
```

`tc` calls it only when the text contains `---`, after normalising `\r\n` and `\r` to `\n`. Its rules:
- **Opener:** the text starts with zero or more whitespace characters that aren't a newline (JS `\s`: it includes U+FEFF, U+00A0 and the Unicode spaces, and not U+0085), then `---`, then only whitespace up to a newline. The leading run is the indent `n`. A blank first line, or any text after `---` on its line, means no front matter.
- **Closer:** a newline, then exactly the opener's indent `n` (character for character), then `---`, then only whitespace that includes at least one newline (so `---` at the very end of the text, with no newline after it, is not a closer). Trailing whitespace and blank lines after the closer belong to the front matter. The first such closer after the opener closes it.

**Whether the anchor's rule matches `Rw`.** The brief's rule ("the opener is the first non-blank line; it opens iff `trim_ws == "---"`; it closes at the next later line whose `trim_ws == "---"`") does not. Differences, each seen through Pele (probe P5):
1. **No blank line before the opener.** `Rw` needs the opener on line 1. With `\n---\ntitle: [x\n---\nclassDiagram` Pele sees no front matter (`unsupported-diagram`). The anchor must open front matter only on line 1 (N3).
2. **The closer repeats the opener's indent,** BOM and tabs included. `  \t---` / `  \t---` is front matter (invalid YAML reported); `  \t---` / `---` is not. A BOM counts as indent: `\u{feff}---` closed by a plain `---` is not front matter, while `\u{feff}---` closed by `\u{feff}---` is. The anchor must compare the closer's prefix with the opener's prefix, not trim.
3. **A closer needs a newline after it.** `---\ntitle: [x\n---` with no final newline isn't closed (`unsupported-diagram`). The anchor's "closed" must require a newline after the closer.
4. **Trailing text on the opener** (`--- x`) is not an opener; trailing whitespace on either line is fine (`---   ` closes).
5. The same JS `\s` set decides all of it, with U+0085 not whitespace (N4): `\u{85}notADiagram` is still an unsupported diagram.

The committed fixtures behave identically under both rules; the differences matter for the anchor tests in s2-3-adapter (they follow `Rw`, and the anchor's definitions in the brief are adjusted as above).

**Nested outcome (P10):** `nested_subgraphs(1_000)` at the default caps (and the adapter's options) answers `Success` in 19 to 27 ms (`svg.length` 283,701), so no depth reduction is needed. `function f(n){return n?f(n-1)+1:0};f(1e6)` throws `RangeError: Maximum call stack size exceeded`; no native crash, and `class.mmd` renders afterwards.

**No carried-over state (P12):** on one runtime, `class.mmd`, the 5 broken fixtures and `class.mmd` again give identical results; a second pass over the 5 broken fixtures gives identical results; each equals its result on a fresh runtime.

## Limits

- **204,800 bytes** (200 KiB). Pele's `limit` counts UTF-16 units of the JS string, which is never more than the source's UTF-8 byte length, so `limit: 204800` admits every source the byte limit admits. P6: `pad_to(class.mmd, 204_800, 'x')` (204,800 units) answers `limit` ("Diagram source is longer than the limit of 50000 characters.") without options, and renders (`svg.length` 4,158) with `limit: 204800`. With `é` as the fill the source is 204,800 bytes but 102,526 units; it answers the same `limit` error without options and renders with `limit: 204800`.
- **`PELE_MAX_EDGES` 51,200.** `fan_out(226)` spells 51,076 edges, under it.
- **`PELE_OUTPUT_LIMIT` 4,000,000,** Pele's default. The largest SVG seen is 1,826,043 characters (the 200 KB diagram). `fan_out(226)`, given enough memory, answers `limit` "Diagram output is longer than the limit of 4000000 characters." after about 6 s in QuickJS (checked: 6.1 to 6.4 s at 132 to 512 MiB).
- **Time-cap overshoot:** the busy loop with a 1 s deadline stops after 1,000.0 ms (overshoot under 0.1 ms); `fan_out(226)` with a 10 ms deadline stops after 10.6 ms (and after 11.4 ms in a dev run), the 200 KB diagram after 10.1 ms, the regex after 200.1 ms.
- **R10:** the cap is the QuickJS heap, measured with `memory_usage().malloc_size`; process RSS (225,744 KiB before the cycles, 269,648 KiB after) is for information only.

## Performance (probe; preliminary, the authoritative numbers are in docs/m2-s2-bench.md)

All under `heavy.sh`, on one machine, release build.

| Metric | Value |
| --- | --- |
| Release build (P1) | 15.7 s compile (cargo `Finished`), plus 15 min waiting for the lease |
| Cold load x5 (P2), strict global eval, 64 MiB cap | 37.3, 36.5, 36.2, 36.1, 36.2 ms (run 1); 35.9, 35.5, 35.3, 35.3, 34.7 ms (run 2, a rerun of P2 inside the P9 run) |
| Engine heap after load | `malloc_size` 3,992,048 and `memory_used_size` 3,385,909, the same before and after `run_gc` (nothing to collect) |
| `class_diagram(50)` (6,661 bytes) warm, 3 discarded then 30 timed | p50 4.2 ms, p95 4.5 ms (min 4.1, max 4.5); first run 4.3 ms; `svg.length` 59,155 |
| `class_diagram_of_bytes(204_800)` warm, 3 discarded then 20 timed | p50 138.2 ms, p95 139.6 ms (min 136.2, max 139.9); first run 145.9 ms; `svg.length` 1,826,043 |
| t_class (median of 5 warm `class.mmd`) | 0.43 ms |
| t_first (max of the first check on 5 fresh runtimes) | 0.50 ms |
| W (smallest cap at which the warm-up succeeds) | 9 MiB (fails at 8 MiB) |
| R50 (smallest cap at which `class_diagram(50)` answers `SUCCESS`) | 9 MiB |
| R200 (the same for the 200 KB diagram) | 19 MiB |
| Process RSS | 225,744 KiB before the 20 cycles, 269,648 KiB after |

p50 is the median; p95 is the value at rank ceil(0.95 n) of the sorted samples.

## Test inputs

Generators are those of the brief's `diagram_support` (`class_diagram`, `class_diagram_of_bytes`, `pad_to`, `nested_subgraphs`, `fan_out`). `class_diagram(50)` is 6,661 bytes, SHA-256 `b06519def05745bb06b7a33389899728dbf71ff3fbea4142a9e4a99426980ee9`. `class_diagram_of_bytes(204_800)` is 204,800 bytes, SHA-256 `8f59fa859369d2970c37d1535aefa860b097979be21812c8bddb8863a63fc3f0`. `fan_out(226)` is 2,957 bytes, SHA-256 `aa5277c930da858a80ab32aa5f139688dc7897f95f9b7152f8b20e41ef479108`. A class block is 115 bytes, not the 117 the first draft of the probe assumed; the probe's arithmetic length is asserted against the generated string.

**P7, the time input.** Warm runs at the default caps (a run that hits the 10 s cap counts as 10 s). t_class 0.43 ms and t_first 0.50 ms, so 80 x max = 40.2 ms.

| Candidate | 3 warm runs (ms) | t_slow_min | Meets >= 80 ms and >= 40.2 ms |
| --- | --- | --- | --- |
| `fan_out(226)` | 111.8, 121.0, 118.9 (each ending in a thrown `null`, an out of memory: amendment A2) | 111.8 ms | yes: **the pick** |
| `class_diagram_of_bytes(204_800)` | 140.6, 138.4, 139.2 (all `Success`) | 138.4 ms | yes (not picked: the first candidate that meets the rule wins) |

**P8** with the pick: `fan_out(226)` under a 10 ms deadline is interrupted after 10.6 ms: `InternalError`, message `interrupted`, flag set.

**P9, the memory input.**
- W = 9 MiB (the warm-up fails at 8 MiB). h = 1 MiB: a fresh runtime capped at W + h = 10 MiB answers `Success` for `class.mmd` as its first check.
- R per candidate: the smallest cap at which the check, under the default 10 s time cap, answers anything other than `MemoryLimit`, bisected at 1 MiB steps up to 512 MiB, with amendment A1 applied (a thrown `null` is `MemoryLimit`):

| Candidate | R | Meets R >= W + h + 2 = 12 MiB |
| --- | --- | --- |
| `fan_out(226)` | 132 MiB (`Mem` at 131 MiB after 5.5 s; `limit`, the output limit, at 132 MiB after 6.2 s) | yes: **the pick** |
| `class_diagram_of_bytes(204_800)` | 19 MiB (`Mem` at 18 MiB; `Success` at 19 MiB) | yes |

- **Confirmation:** `fan_out(226)` at W + h = 10 MiB with a 60 s time cap answers `MemoryLimit` after 8 ms (a thrown `null`). Without A1 the same run is classified `Other`, "null" (run 1: R(`fan_out(226)`) = 77 MiB, with `Other` at every cap from 77 MiB to 131 MiB; the 200 KB diagram's R is 19 MiB either way), which is why A1 is needed.
- **The wrapped error:** Pele doesn't wrap the out-of-memory error. The value is `InternalError: out of memory` or `null` (A1), both with no `code`.
- **R50** is 9 MiB and **R200** is 19 MiB (smallest caps at which the two answer `SUCCESS`).

## Tooling

Measured from the slice worktree root (the same checkout `./tools/verify` runs in), 2026-11-24.

| Check | Before vendoring | After vendoring | Result |
| --- | --- | --- | --- |
| `python3 -I tools/arch-guard/check.py` | 0.57 s | 0.62 s | `arch-guard: clean` both times |
| `trellis rules check` | 2.9 s (first, cold index) | 1.1 to 1.3 s | exit 0 both times; deviates 0, excepted 0, unknown 290, unassigned files 2 in both (the JSON differs only in revision ids and the index revision) |

`check_js_file` accepts `pele-1.13.0.global.js`, and trellis reads it in the adapter-loader layer without a finding. Nothing was renamed, skipped or excepted, and `guard.toml`, `check.py` and `rules.toml` are untouched.

Driver's pre-dispatch grep at 6a074ce: ten files hit, none a closed inventory over the new paths, so no slice's file list changed.

## Fallbacks

None. The deno_core probe was not needed: G3, G4 and G5 are go on rquickjs, and `render()` works, so parser-only mode isn't needed either.

## Known limits

- A running regex **is** interrupted (P8: 200.1 ms against a 200 ms deadline), so there is no watchdog case and no watchdog issue was filed.
- QuickJS fails an allocation at the cap without collecting garbage first, so a validation can reach the cap with collectable garbage still live. The check that reaches the cap is answered `MemoryLimit` and its runtime is rebuilt.
- QuickJS signals some out-of-memory failures as a thrown `null` rather than an `InternalError` (amendment A1); the engine heap read after the unwound call doesn't show them.
- There is no in-flight cancellation (S3).
- The cap covers the engine heap, not the process.

## How the probe was built

A throwaway binary crate under `/tmp/c45s/probe`, outside the checkout: `name = "c45-probe"`, `edition = "2021"`, `publish = false`, an empty `[workspace]`, and one dependency, `rquickjs = { version = "=0.14.0", default-features = false, features = ["std"] }`. It was built with `cargo build --release` under `heavy-medium.sh` and a private `CARGO_TARGET_DIR`, and run under `heavy.sh` (the large tier) as the outermost command, once for every phase and once more for P9 after the `null` finding. It took the bundle and the fixtures directory as arguments and contained the brief's generators.

Every runtime was built as the adapter will build it: on an 8 MiB thread, `set_memory_limit(64 MiB)` (or the cap under test), `set_max_stack_size(1 MiB)`, an interrupt handler that compares a deadline in nanoseconds since the runtime's origin and sets a flag, `Context::full`, the bundle evaluated with `eval_with_options` (`global = true`, `strict = true`), a check that `pele.render` and `pele.parse` are functions, and `run_gc`. Thrown values were read as in M11. Pele was called as `render(source, { limit, maxEdges, outputLimit })` with the source as a string argument.

Steps: P1 timed the build; P2 built 5 runtimes and read `memory_usage()` before and after `run_gc`; P3 evaluated `Object.keys(pele)` and the `typeof` checks and counted the global names in the bundle; P4 and P5 called `render` and `parse` on `class.mmd`, the five fixtures, variant B of two of them and a set of probes of the front-matter detector and the column unit; P6 padded `class.mmd` to 204,800 bytes with `x` and `é`; P7 timed warm runs, t_class, t_first and the two time candidates; P8 ran a busy loop, the slow inputs and a catastrophic regex under short deadlines; P9 ran an allocation loop, bisected W, stepped h, bisected R per candidate and R50 and R200, and confirmed the memory input at W + h; P10 rendered `nested_subgraphs(1_000)` and a runaway recursion; P11 ran 20 cap-and-rebuild cycles; P12 compared results across a fixture sequence on one runtime and on fresh ones; P13 set an interrupt handler that captures an `Rc<Cell<u64>>` (it compiles, so `set_interrupt_handler` has no `Send` bound without `parallel`) and noted that `Module::load` is `unsafe`. A short diagnostic run caught the exception inside JS to show that the out-of-memory value is `null`. Quick correctness runs of the cheap phases were made without the lease and none of their timings is recorded here. The probe's source was not kept; it and the build directory were deleted at the end.
