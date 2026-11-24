# M2-S2 benchmark: headless Mermaid validation through the adapter

Checkpoint B of run #45 (s2-3-adapter). The result is **go**: the adapter on rquickjs meets both thresholds, so there is no fallback.

- **Command:** `sh tools/diagram-bench` (exit 0). It takes the large tier of the shared heavy-run lease (`heavy.sh`) itself, and it was started outside any other lease, so the output has no "started inside another heavy-run lease" warning.
- **Commit:** 36df128 (the adapter, its wiring, the tests and `tools/diagram-bench`); this file is added after it.
- **Date:** 2026-11-24.
- **Machine:** Apple M5 Pro (Mac17,16), macOS 27.2, aarch64-apple-darwin; rustc 1.98.1 (48a229cea 2026-09-01); release profile.

## New Cargo.lock packages

Cargo.lock gains five packages and changes nothing else. None is under 14 days old, so none needed `cargo update --precise`.

| Package | Version | Published |
| --- | --- | --- |
| rquickjs | 0.14.0 | 2026-09-18 |
| rquickjs-core | 0.14.0 | 2026-09-18 |
| rquickjs-sys | 0.14.0 | 2026-09-18 |
| relative-path | 2.0.3 | 2026-10-21 |
| allocator-api2 | 0.2.21 | 2024-12-01 |

`[profile.dev.package.rquickjs-sys] opt-level = 3` takes effect: cleaning `rquickjs-sys` and running `cargo build -vv -p combined` shows its build script run with `OPT_LEVEL=3`. (The `cc` crate doesn't print its compiler line, so `grep -- '-O3'` finds nothing; `OPT_LEVEL` is what it reads.)

## Inputs

| Input | Bytes | Classes | SHA-256 |
| --- | --- | --- | --- |
| `class_diagram(50)` | 6,661 | 50 | `b06519def05745bb06b7a33389899728dbf71ff3fbea4142a9e4a99426980ee9` |
| `class_diagram_of_bytes(204_800)` | 204,800 | 1,536 | `8f59fa859369d2970c37d1535aefa860b097979be21812c8bddb8863a63fc3f0` |

Both equal the values in docs/m2-s2-spike.md, "Test inputs" (the generators are asserted against them in `tests/diagram_check.rs::generators_match_the_spike_inputs`, together with `fan_out(226)`).

## Results

Every timed verdict is `Validated`. Times are milliseconds; p50 and p95 are nearest-rank.

| Metric | Input | Value |
| --- | --- | --- |
| Cold load (5 fresh checkers, to `finished()`) | | min 39.9, p50 44.4, max 47.5 |
| First check on a fresh checker | 50 classes | 5.5 |
| Warm check, 3 discarded then 30 timed | 50 classes | min 5.1, p50 5.1, p95 5.3, max 5.5 |
| Warm check, 3 discarded then 20 timed | 200 KB | min 139.5, p50 144.3, **p95 154.4**, max 155.5 |
| Rebuild after a time cap (next small check minus a warm small check) | | 36.0 |
| Heap after warm-up | | 3,992,048 bytes |
| Largest heap after a check | 50 classes | 3,999,704 bytes |
| Largest heap after a check | 200 KB | 3,999,704 bytes |
| Smallest memory cap, warm-up | | 9 MiB |
| Smallest memory cap that answers `Validated` | 50 classes | 9 MiB |
| Smallest memory cap that answers `Validated` | 200 KB | **19 MiB** |
| Process RSS after warm-up | | 21,376 KiB |
| Process RSS after the 200 KB runs | | 36,880 KiB |

`diagram_bench go=true warm_p95_200_kb_ms=154.4 limit_ms=5000 smallest_cap_200_kb_mib=19 limit_mib=64`

The heap read after a check is taken once the call has returned and been unwound, so it is not the peak: both inputs read about 4 MB, while the smallest cap that validates the 200 KB diagram is 19 MiB. The caps are what bound the peak. The 200 KB warm p95 here (154.4 ms) is a little above the spike probe's 139.6 ms, which was a different binary; both are far under the 5,000 ms limit.

## Raw output

From `sh tools/diagram-bench` (exit 0), the `diagram_bench` lines verbatim. Cargo prints `test diagram_validation_benchmark ... ` before the first one, which is left out.

```text
diagram_bench header engine=rquickjs pele=1.13.0 verdict=validated profile=release
diagram_bench input name=50_classes bytes=6661 classes=50 sha256=b06519def05745bb06b7a33389899728dbf71ff3fbea4142a9e4a99426980ee9
diagram_bench input name=200_kb bytes=204800 classes=1536 sha256=8f59fa859369d2970c37d1535aefa860b097979be21812c8bddb8863a63fc3f0
diagram_bench metric=cold_load unit=ms n=5 min=39.9 p50=44.4 max=47.5
diagram_bench metric=first_check input=50_classes unit=ms value=5.5
diagram_bench metric=warm_check input=50_classes unit=ms n=30 min=5.1 p50=5.1 p95=5.3 max=5.5
diagram_bench metric=warm_check input=200_kb unit=ms n=20 min=139.5 p50=144.3 p95=154.4 max=155.5
diagram_bench metric=rebuild_after_cap unit=ms value=36.0
diagram_bench metric=heap_after_warmup unit=bytes value=3992048
diagram_bench metric=heap_after_check input=50_classes unit=bytes max=3999704
diagram_bench metric=heap_after_check input=200_kb unit=bytes max=3999704
diagram_bench metric=smallest_cap input=warmup unit=MiB value=9
diagram_bench metric=smallest_cap input=50_classes unit=MiB value=9
diagram_bench metric=smallest_cap input=200_kb unit=MiB value=19
diagram_bench metric=rss input=after_warmup unit=KiB value=21376
diagram_bench metric=rss input=after_200_kb unit=KiB value=36880
diagram_bench go=true warm_p95_200_kb_ms=154.4 limit_ms=5000 smallest_cap_200_kb_mib=19 limit_mib=64
```

## Where this slice differs from docs/m2-s2-spike.md and the brief

docs/m2-s2-spike.md belongs to s2-1-spike and is not edited. Nothing in its fixture table is superseded: `tests/diagram_check.rs::broken_fixtures_fail_with_a_position` asserts the five rows (code, line, column and snippet) exactly, twice over one runtime. These points are decisions of this slice.

- **Shims:** none, as P3 found. `console` and `performance.now` are not installed.
- **Anchor follows `Rw`, not the brief's prose.** `pele::anchor` ports `Rw` from the quote in the spike (the opener only on line 1, the closer repeating the opener's indent, a newline after the closer, and `Rw`'s closer selection: `o` the last closer past `r[0]`, `c` the last newline of the opener's white-space run before `o`, the first closer after `c`). Line endings are normalised to `\n` first, as Pele does, so a lone `\r` is a newline. The brief's "first non-blank line opens, next later `---` closes" is not used.
  - "Unclosed" means a valid opener for which `Rw` finds no closer. That includes `---`, `---`, `classDiagram`, where a `---` follows on line 2 but is never a closer. For `UnsupportedDiagram` the answer is the same as the spike's rule 2 reading (line 1, column 1, `---`). For `Syntax` and `Semantic` with no Pele position, the anchor takes rule 1 for such a source; the spike's prose says only that rule 1 is not taken for the unsupported case.
  - A byte-order mark is dropped from the snippet and not counted in the column wherever it sits in the line's leading white space; a mark after the first non-white-space character is left alone.
- **Pele-confirmed front-matter cases** run through the adapter in `tests/diagram_check.rs::front_matter_anchors_agree_with_pele` (the spike's blank-line cases, a line-2 closer, a leading blank line, a BOM-indented opener), and at the anchor in `pele.rs::tests::front_matter_is_chosen_as_pele_chooses_it`.
- **Stack overflow inside Pele:** `pele.rs::tests::stack_overflow_inside_pele_is_pele_s_limit_verdict` pins the wrapped shape (a `limit` `PeleError`, "Diagram is nested too deeply.", rule 5, runtime kept). No engine-level test raises it: 9,000 nested subgraphs validate, and a 200 KB source cannot nest deeper. The bare `RangeError` (rule 3) is covered by `engine.rs::tests::deep_recursion_is_a_limit_and_the_runtime_is_rebuilt`.
- **Thrown `null`** is detected from the value (`Thrown::thrown_null`), never from its text: `a_thrown_null_is_a_memory_limit` and `a_thrown_string_null_is_no_verdict` in `pele.rs`, and the same pair at the engine.
