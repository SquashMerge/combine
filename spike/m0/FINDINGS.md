# M0 findings: terminal fidelity over herdr-core

herdr-core 0.1.21 (protocol generation 1), macOS (Darwin 27.2), one isolated core (`HC_SESSION=m0`, core id `a4366648...`), never the user's default core. Spike code: `spike/m0`; README.md says how to rerun everything. Results JSON: `spike/m0/results/` (`*-ssh*` = over real ssh). Wire captures and paint dumps: `spike/m0/results/captures/`. Run on 2026-11-20.

Note on the result files: each `gonogo-*.json` case records `pass: false` whenever any field differs, including the losses and harmless differences listed in section 2. The verdict below applies SPEC's criteria to those differences; every recorded difference maps to an item in section 2.

## Verdict

**Go, with recorded losses and two design changes.**

- **Go/no-go (SPEC's criteria): pass in all three cases.** A fresh `terminal.attach` restores:
  - screen text and colours;
  - cursor position and visibility;
  - the alternate screen, with the main screen kept behind it;
  - mouse tracking (1000/1002/1003) with SGR encoding (1006), focus reporting (1004), bracketed paste (2004);
  - application cursor keys (DECCKM) and application keypad.

  That holds for (a) a terminal started before the daemon, (b) after a daemon restart and (c) after `hc server restart`. The first paint was byte-identical before and after the restart, and `Terminal.screen` stayed `exact`.
- **Not everything survives.** The first paint drops: the DECSC saved cursor, the DECSCUSR cursor shape, soft-wrap flags, OSC 8 links on cells already written, OSC 4 palette and OSC 10/11 colour changes made by the program, and alternate-scroll mode (`?1007`). Each is a decision for the owner (section 7).
- **Two SPEC assumptions are wrong**, and Combine needs a design change for each:
  1. **Size.** Only `observe` viewers get `resize_notifications`, and with it RESIZE plus a replacement paint. An active viewer is never told when another viewer changes the PTY size; it just starts getting bytes drawn for a size it doesn't have. A workaround works today and was tested: pair each panel's observe viewer (display) with an input-only active viewer.
  2. **Flow control.** herdr-core buffers about 4 MiB per viewer. When a viewer stops granting credit, the core drops the backlog and sends a fresh in-band repaint; it never stalls. So the daemon's "grant credit only as the client drains" doesn't create a Gap. It turns into a core-side resync that the daemon can't see.
- **Relay contract:**
  - Pass: replay without loss or duplication, resize around a drop, backpressure and Gap, `hc attach` alongside. All of these also pass over real `ssh -T -o BatchMode=yes localhost`.
  - Fail: two active attachments of different sizes (point 1 above).
- **Queries: partial.** herdr-core answers and strips 21 of the 28 queries tested. Seven reach the viewer unanswered.
- **Latency:**
  - p99 is 0.5 ms or less on every local path, including ssh to localhost, against a 30 ms budget.
  - LAN is untested.
- **50 MB burst:**
  - A byte-counting client got everything at 75 MB/s.
  - A client applying to `@xterm/headless` could not keep up, and herdr-core skipped ahead with in-band repaints (history lost, state correct).

## 1. herdr-core's framed protocol (observed)

How it was found:
1. A logging Unix-socket proxy (`tools/proxy.py`) between `hc attach` running under a pty (`tools/pty_attach.py`, `HC_SOCKET` pointed at the proxy) and the isolated core.
2. Then a Python client (`tools/hcframe.py`, probes in `tools/probes/`) for the rest.

Captures: `results/captures/attach1.log`, `attach2.log`. They hold one hex line per chunk; the hex below is copied from them and from probe output.

### Handshake

`hello` and its reply are plain newline-terminated JSON-RPC lines. Framing starts with the next byte, in both directions:

```
C>S  {"jsonrpc":"2.0","id":0,"method":"hello","params":{"framed":true}}\n
S>C  {"id":0,"jsonrpc":"2.0","result":{"core":{...},"features":["framed","attach","events","status","shell",
      "resize_notifications","automatic_input","restart_environment"],"framed":true,"generation":1,...}}\n
```

### Frame layout

```
u32 LE channel | u8 kind | u32 LE payload length | payload
```

Channel 0 carries JSON-RPC: kind 0, one JSON message per frame. Example: `00000000 00 c7000000 7b226964223a31...` is the 199-byte `terminal.attach` request. `AttachResult.channel` names the attach channel; the first attach on a connection got 1. Kinds on an attach channel:

| kind | name | direction | payload | evidence (hex) |
| --- | --- | --- | --- | --- |
| 0 | DATA | both | core to viewer: paint and output. viewer to core: keys | `01000000 00 03000000 1b4f41` (viewer sends `ESC O A`). Paint: `01000000 00 2d040000 18 1b5b2170 1b5b3f323032366c ...` |
| 1 | CREDIT | viewer to core | u32 LE **bytes** | `01000000 01 04000000 2d040000` (credits 1069 = the paint just received) |
| 2 | CLOSE | both | viewer: empty (detach). core: JSON `{exit_code,finished_ms,reason,signal}` when the program ends | viewer: `01000000 02 00000000`. core: `01000000 02 4b000000 7b22657869745f636f6465223a6e756c6c...` = `{"exit_code":null,"finished_ms":...,"reason":"killed","signal":1}` |
| 3 | RESIZE | both | u16 LE cols, u16 LE rows | viewer: `01000000 03 04000000 6400 1e00` (100x30). core to observer: `01000000 03 04000000 5000 1800` (80x24) |
| 4 | FOCUS | viewer to core | u8: 1 in, 0 out | `01000000 04 01000000 01` |
| 5 | FACTS | viewer to core | JSON Display | `01000000 05 40000000 {"fg":"#ddeeff","bg":"#112233","cursor":"#aabbcc","cell":[9,18]}` |
| 6 | AUTOMATIC_INPUT | viewer to core | bytes | from the schema. Accepted from an active viewer, silently dropped from an observer |

`hc attach` asks its outer terminal on start: OSC 10/11/12, `CSI 16 t`, DA1. It sends FACTS once it has the answers. It also turns on `?1004` there and sends FOCUS on focus reports.

### Credit

- **Unit:** bytes of DATA payload. `hc attach` credits back each DATA frame's length as it receives it.
- **Initial allowance:** **262,144 bytes (256 KiB), first paint included.**
  - An attach that never sent CREDIT received a 248-byte paint plus 261,896 bytes of output, exactly 262,144, then nothing for 2 s.
  - A CREDIT of 100,000 then released exactly 100,000 more bytes (`tools/probes/exp_credit.py`).
- **Credit never makes the core wait.** It stops sending to that viewer and queues up to about 4 MiB for it. Past that, it **drops the queue and sends a fresh paint in-band**, then carries on live.
  - Evidence (`exp_stall.py 20 4`): a 20 MB burst with no credit for 4 s.
  - After crediting, the viewer got lines 0..4091, then a full paint (`CAN ESC[!p...`) whose screen began at line 266169, then 3,936,964 bytes of live output up to the last line.
  - Lines 4092..266168 were never sent.
  - There was exactly one repaint, however much was dropped.
- **Frame size:** core-to-viewer DATA frames are at most **16,384 bytes**. Paints over 16 KiB span several frames.

### First-paint boundary

- **The start is visible.** Every paint begins with `CAN` (0x18), then `ESC [ ! p` (DECSTR), then resets of every mode. That covers the first paint, the resync paint and observer replacement paints.
- **The end is not marked** by any frame, kind or flag.
  - In practice every paint ends with `CSI ? 9001 l`. That held for all 8 first paints dumped in `results/captures/paint-*.bin` and for every resync and replacement paint seen.
  - It's undocumented, so don't rely on it.
  - There is no synchronized output (`?2026h`) around the paint, so a viewer can draw half a paint.
- **Paints with history are slow and capped:**
  - `history: 1000` took 5 frames over about 300 ms.
  - `history: 10000` gave 180,799 bytes in 12 frames over 1.1 s, which is fewer than 10,000 lines.

### `resize_notifications`

- Every viewer asked for it. **It is granted only to `observe` viewers**: `AttachResult.resize_notifications` was `true` for observe and `false` for active, in every run.
- **An observer** gets `RESIZE` before its first paint. On every PTY size change it gets `RESIZE`, then a full replacement paint, ahead of the program's own redraw. Checked with vim through resizes caused by another viewer's attach, resize, focus and detach (`exp_obs2.py`).
- **An active viewer** gets **no** RESIZE and no replacement paint when another viewer changes the size. It only gets the program's redraw, drawn for the new size.

### Viewer arbitration (what herdr-core does)

- **Last active wins the PTY size.** An active viewer's attach sets the size immediately; so do `FOCUS 1`, typing and RESIZE.
- **Observers:** their DATA, AUTOMATIC_INPUT and RESIZE are silently dropped, with no error.
- **`resumed` viewers** set the size only once they type or focus.
- **When the last active viewer leaves,** the PTY goes back to the configured headless size (default 200x120). The program gets a SIGWINCH on every last detach.
- **Display queries** are answered from the most recently active viewer's `display`: A's colours and size after A focused, B's after B focused (relay "two").

## 2. Go/no-go detail

**Oracle:** a reference `@xterm/headless` 6.0.0 fed the program's entire output since spawn, compared field by field with a fresh `@xterm/headless` fed only what the attachment delivered. The fixtures write a trace; `tee_pty.py` traces vim and less.

Compared:
- every visible cell: char, width, fg/bg colour and mode, bold, dim, italic, underline, inverse, blink, invisible, strike, overline;
- the main-screen rows behind the alt screen;
- active buffer, cursor, cursor visibility and shape;
- all modes xterm.js exposes, plus mouse encoding;
- scroll region, saved cursor, SGR pen, charset, tab stops, title.

Both emulators use Unicode 11 widths (see the width note). The core's headless size was set to 80x24, so the reference needs no resizes.

**Fixtures** (in `fixtures/`):
- `modes.py`, an alt-screen app. It sets mouse 1000+1002+1006, focus 1004, bracketed paste, DECCKM, DECKPAM, DECSCUSR 4, a hidden cursor, DECSTBM 3;20, DECSC at (15,5), a bold magenta pen left on and an OSC 2 title. It writes SGR bold/italic/underline/256/truecolour, wide CJK, emoji, a combining mark, a flag and a BCE erase, with main-screen lines underneath.
- `modes2.py`, main-screen state: G0 DEC graphics, `?7l`, DECOM with DECSTBM 5;15, IRM, `?45`, any-event mouse + SGR, `?1007`, `?12`, kitty keyboard flags `CSI > 1 u`, modifyOtherKeys 2, OSC 4, OSC 10, an open OSC 8 link, curly underline and an underline colour.
- vim: `vim -u NONE -N +'set mouse=a' +'set number' +'syntax on'`.
- less: `less -R +15`.
- htop isn't installed.

**Commands:** `node tests/gonogo.mjs modes|modes2|vim|less`. Results: `results/gonogo-*.json`. Over ssh (cases a and b): `results/gonogo-modes-ssh-norestart.json`, identical to the local run.

### What survives, per case

Each `x` means the reference and the fresh attach matched for that state in that case.

| State | a: before daemon | b: daemon restart | c: server restart, daemon re-attach | c: fresh attach |
| --- | --- | --- | --- | --- |
| Screen text, colours, SGR attributes, wide chars, emoji | x | x | x | x |
| Main screen behind the alt screen (checked live by leaving it) | x | x | x | x |
| Cursor position and visibility | x | x | x | x |
| Active buffer (alt screen) | x | x | x | x |
| Mouse 1000/1002/1003, SGR 1006 encoding | x | x | x | x |
| Bracketed paste 2004, focus 1004 | x | x | x | x |
| DECCKM, application keypad (sent as `?66`; DECKPAM in the source) | x | x | x | x |
| IRM, DECOM, DECAWM off, reverse wrap `?45`, cursor blink `?12` | x | x | x | x |
| Scroll region (DECSTBM) | x | x | x | x |
| SGR pen left active (incl. curly underline, underline colour) | x | x | x | x |
| G0 charset (DEC graphics) | x | x | x | x |
| Title (OSC 2) | x | x | x | x |
| Kitty keyboard flags, modifyOtherKeys 2, OSC 8 link left open | in the paint bytes (`=1;1u`, `>4;2m`, OSC 8); xterm.js can't represent them | same | same | same |
| **DECSC saved cursor** | lost | lost | lost | lost |
| **DECSCUSR cursor shape** | lost | lost | lost | lost |
| **Soft-wrap flag on wrapped rows** (vim, less) | lost | lost | lost | lost |
| **OSC 8 link on cells already written** | lost | lost | lost | lost |
| **OSC 4 palette, OSC 10 default colour set by the program** | not in paint | not in paint | not in paint | not in paint |
| **`?1007` alternate scroll** | not in paint | not in paint | not in paint | not in paint |

**(b) Daemon restart.** Resuming with the old cursor gets `{"op":"gap","reason":"unknown_attachment"}`. A fresh attach is then identical to (a).

**(c) Server restart.**
- The daemon's stream ends with EOF, not CLOSE.
- The daemon re-attaches with `resumed: true` and starts a new epoch.
- The client gets an `attached` event for epoch 2 (with `screen: "exact"`), discards its xterm, and a new one fed only the new paint is identical to (a).
- Re-attach took 0.9 to 1.1 s after `hc server restart` returned. The daemon just retries attach every 250 ms; it doesn't use `events.subscribe` `restarted`.

**Live checks after the paint** (`modes`, case c), to see what the losses do to later output:
- `p` (write with the current pen): correct.
- `s` (scroll inside the region): correct.
- `w` (write at the margin): correct.
- `r` (DECRC, then "RC"): **wrong**. The fresh attach has no saved cursor, so "RC" lands at (0,0) instead of (14,4) and overwrites the header.
- `q` (leave the alt screen): the main screen is correct. The only diff is that xterm.js keeps the alt screen's scroll region on the main screen. The paint sends DECSTBM for both screens because Ghostty has one region for the terminal, while xterm.js has one per buffer. That's an emulator difference, not a loss.
- vim `:q!` and less `q`: the main screen is restored correctly.

**Benign differences** (`relay.mjs` filters them out):
- **Tab stops:** the paint sets 8, 16, …; xterm.js's default set also has column 0, which a tab can't reach.
- **BCE erase:** `ESC[44m ESC[K` is painted as spaces with a blue background. It looks identical, but a copy picks up trailing spaces.

**Width tables** (a Combine requirement, not a herdr-core loss):
- With `@xterm/headless`'s default Unicode 6 widths, vim's emoji rows were one column off after a fresh attach.
- Ghostty treats 🎉 as 2 columns, and the paint relies on the terminal advancing 2.
- With `@xterm/addon-unicode11` (pinned 0.9.0) it matches.
- Combine must use herdr-core's Ghostty widths, and test grapheme clusters (flags, ZWJ sequences, mode 2027).

**`terminal.read`:**
- It matched the reference text for both state fixtures.
- For vim and less it returns logical lines (a soft-wrapped row is joined to the next), so it doesn't compare row for row. That's expected.

**`Terminal.screen`** was `exact` everywhere. The core-crash (`inexact`) path wasn't tested; SPEC tests crash recovery separately.

## 3. Relay contract (m0d)

`m0d` (`spike/m0/m0d`, about 750 lines of Rust on tokio) attaches once per client attachment. It relays opaque DATA plus `resized`, `attached`, `closed`, `core_lost` and `lost` events as one ordered log per attachment, each with a cursor `(att, epoch, seq)`.
- The client acks the last event it **applied** to its xterm.
- The daemon keeps everything after the last ack, up to `--log-cap` (default 4 MiB).
- A dropped client resumes from its own last-applied cursor within a 30 s grace period.
- Credit policy `ack` (default) grants herdr-core credit only while log plus in-flight stays under the cap. Policy `eager` credits every byte and answers Gap when the log overflows.
- Modes: `active` uses one herdr-core viewer. `paired` uses an observe viewer for the stream plus an input-only active viewer.
- The protocol is described in the header of `m0d/src/main.rs`.

Commands: `node tests/relay.mjs [test]` (`M0_VIA=ssh` for real ssh). Results: `results/relay*.json`.

| Test | Result | Evidence |
| --- | --- | --- |
| No lost or duplicated bytes across disconnect/reconnect during heavy output, inside the window | **pass**: 3/3 runs local, 2/2 over ssh | A 3 MB burst of numbered 64-byte lines. The connection dropped at seq 39–101 and stayed away 60 ms, then 9 more drops. All 49,152 lines arrived once, in order, with no seq gaps or duplicates. The final screen matched a fresh attach. |
| Away longer than the window | **pass** for state; output history is lost by design | A 16 MB burst, away 4 s. The daemon log stopped at exactly 4,194,304 bytes with 0 outstanding credit. Resume replayed it, then herdr-core's one in-band repaint (about 131,000 lines skipped), then live. The final screen matched a fresh attach. No Gap was needed. Same over ssh. |
| Drop and replay just before or just after a resize | **pass**, all 4 variants, local and ssh | vim. **Paired:** A dropped and B resized to 100x30; or B resized to 90x20 and A dropped 5 ms later. A resumed and was identical to B, which never dropped. **Active:** A resized then dropped; or A dropped, resumed, then resized. Seqs were continuous, with one `resized` event in the right place, matching a fresh attach. |
| Two attachments, different sizes and display facts | **fail** in active mode, **pass** in paired mode | **Active:** A 80x24, B 120x40. The PTY followed the last active viewer; A's xterm stayed 80x24 while vim drew for 120x40 (`A_resizes: []`). **Paired:** both xterms followed the PTY (80x24 ↔ 120x40 as focus moved) via in-band `resized`. In both modes each viewer's input reached the program, and queries were answered with the focused viewer's colours and size. |
| Slow client: bounded memory, then Gap | **pass** | **`ack`:** a client that stopped reading during a 50 MB burst left the daemon log at 4,194,304 bytes or less, and daemon RSS went from 3.3 MB to a peak of 8.6 MB. When it read again it resynced through herdr-core's in-band repaint, and the final screen matched. Over ssh: peak 12.6 MB RSS, same cap. **`eager`, 1 MiB cap:** 7 overflows, a live `gap` event, `gap` on resume (`outside_replay_buffer`), then a fresh attach. |
| Terminal queries answered by herdr-core | **partial** | See below. |
| `hc attach` alongside a Combine attachment | **pass**, local and ssh | `hc attach` under a pty at 80x24, then 100x30, typed `h`, detached. The paired Combine attachment saw `h` and the resizes 100x30 → 80x24. `hc attach` saw the program's output, and Combine's input still worked afterwards. |
| `resize_notifications` | asked for every time; **granted only to observe** | `false` for every active attach, `true` for the paired observer |

### Terminal queries

Run with `fixtures/queries.py` and `tools/probes/exp_query.py`; results in `results/captures/q_*.json`. The viewer's display was fg `#ddeeff`, bg `#112233`, cursor `#aabbcc`, cell 9x18.

**Answered by herdr-core**; the viewer never saw these queries:
- DA1 (`?62;22c`), DA2 (`>1;10;0c`), DA3, DSR 5n, CPR 6n;
- OSC 10/11/12: the viewer's colours, or Ghostty defaults with no viewer;
- OSC 4 palette queries;
- `CSI 14/16/18/19 t`, answered from the viewer's cell size and size;
- XTVERSION (`ghostty`);
- DECRQM for 2004, 1049 and IRM;
- kitty keyboard `CSI ? u`;
- DECRQSS `m` and `r`.

All of these are answered even with no viewer attached.

**Reached the viewer unanswered.** The program got no reply within 1 s for:
- DECXCPR `CSI ? 6 n`;
- `CSI 21 t`;
- XTGETTCAP `DCS + q`;
- OSC 52 read;
- ENQ;
- DECREQTPARM `CSI x`;
- kitty graphics `APC G a=q`.

Fed to `@xterm/headless`, only DECXCPR gets a reply (`CSI ? 1 ; 1 R`). So with SPEC's rule that clients don't forward xterm.js replies, all seven go unanswered.

## 4. Measurements (whole runs through heavy.sh)

### Keystroke echo latency

`$HEAVY node tests/measure.mjs latency 2000`. 2000 single-byte keystrokes per path to a raw-mode echo program (`fixtures/echo.py`), 3 ms apart. Results: `results/latency.json`.

| Path | p50 | p95 | p99 | max | mean |
| --- | --- | --- | --- | --- | --- |
| herdr-core direct (framed client, no daemon) | 0.151 ms | 0.231 | 0.316 | 5.42 | 0.164 |
| through m0d, Unix socket | 0.190 | 0.281 | 0.380 | 1.29 | 0.195 |
| through m0d, local `m0d connect` pipe | 0.160 | 0.238 | 0.288 | 0.49 | 0.168 |
| through m0d, real `ssh -T -o BatchMode=yes localhost .../m0d connect` | 0.322 | 0.428 | 0.495 | 6.72 | 0.335 |

- The daemon adds about 0.04 ms at p50, and local ssh about 0.15 ms more.
- The 30 ms LAN budget is untested: it needs a second machine (section 8).

### 50 MB output burst

`$HEAVY node tests/measure.mjs burst 50`, through the bridge, `--credit ack`, 4 MiB cap. Results: `results/burst.json`.

| Client | Time | Received | Loss | Daemon RSS before → peak | Core RSS peak |
| --- | --- | --- | --- | --- | --- |
| byte counter, local pipe | 0.66 s (75 MB/s) | 52,428,813 B (all) | none: 819,200 lines, 0 jumps, 0 duplicates, 0 in-band repaints | 3.4 → 4.9 MB | 16.8 MB |
| `@xterm/headless` applying every event, local pipe | program done in 0.73 s | 6,302,404 B | output history lost: 11 in-band repaints, 11 jumps, 0 duplicates; last line received | 3.3 → 10.1 MB | 23.0 MB |
| `@xterm/headless`, real ssh to localhost | 0.70 s | 6,302,404 B | same: 11 repaints | 3.3 → 9.5 MB | 23.0 MB |

How to read it:
- The program writes 50 MB in about 0.7 s and herdr-core never slows it down.
- A client that only counts bytes keeps up.
- A client that parses into xterm.js doesn't. The daemon withholds credit at its 4 MiB cap, herdr-core's own queue overflows, and it repaints in-band.
- Memory stays bounded on both sides, and the client ends on the right screen (same mechanism as the "overflow" test). The bytes in between are never delivered.

### Network listeners

- `lsof -nP -a -p <m0d> -i` lists nothing, so m0d has no network sockets.
- `lsof -U` shows only the 0600 socket `m0d.sock` and its client connection to the core socket.
- `m0d serve --core ~/.local/state/herdr-core/default/default.sock` exits 2 with "refusing the default herdr-core socket".

## 5. M0 items in SPEC.md

| SPEC M0 item | Status | Where |
| --- | --- | --- |
| attach with no lost or duplicated output across hide/show and reconnect during heavy output | **pass** for reconnect (local and ssh). Hide/show wasn't tested separately; it amounts to a reconnect or a fresh attach. | relay "replay" |
| full-screen TUI and alternate screen | **pass**: vim, less, alt-screen fixture. htop not installed. | gonogo |
| resize | **pass** for the resizing client and in paired mode. **fail** for other active viewers. | relay "resize", "two" |
| two clients attached | **pass** paired, **fail** active (sizes) | relay "two" |
| attaching to a full-screen program started before the daemon | **pass** | gonogo case a |
| daemon restart while it runs | **pass**: Gap, then a fresh attach restores state | gonogo case b |
| herdr-core `server.restart` while it runs | **pass**: automatic re-attach in a new epoch, and a fresh attach | gonogo case c |
| an `hc attach` alongside | **pass** | relay "hcattach" |
| keystroke latency | **pass** locally (p99 0.5 ms or less, including ssh to localhost). **not tested** on the LAN. | section 4 |
| 50 MB output burst | **pass** for transport (no loss, bounded memory). With an xterm.js client, output history is lost (in-band resync) and state is correct. | section 4 |
| one xterm.js client over SSH on the local network | **partial**: headless client over real ssh to localhost. LAN and visual client **not tested**. | sections 3, 8 |
| go/no-go: first paint restores screen, cursor, alt screen, input modes | **pass**. Extra lost state is listed. | section 2 |
| pin down frame encodings, credit units, end of first paint | **done** | section 1 |
| no terminal query reaches xterm.js unanswered | **fail**: 7 query types reach it | section 3 |
| real agent calls `show_diagram`, gets a validation error, repairs it; restart restores layout and binding | **not tested** (out of scope for this run) | |
| visual xterm.js client | **not built** | section 9 |

## 6. Where SPEC.md's herdr-core assumptions are wrong or underspecified

1. > "RESIZE arrives before replacement paints only if the returned `AttachResult.resize_notifications` is true; asking for it isn't enough"

   True, but incomplete: herdr-core **grants it only to `observe` viewers**. Under SPEC's "each Combine attachment is its own herdr-core viewer" (Input ownership protocol), an active panel that isn't the owner draws bytes meant for a size it doesn't have.

2. > "two panels on one terminal can differ in size, display and focus"

   This is also the contract test "two attachments on one terminal with different sizes and display facts". The PTY has one size, set by the last active viewer. Panels can *ask* for different sizes, but the bytes are drawn for one size. A panel that isn't the owner must show the owner's size (letterbox, scale or crop). Queries are answered from the **most recently active** viewer's display, not per viewer.

3. > "Backpressure: each attachment has a bounded send queue, and the daemon grants herdr-core CREDIT only as the client drains it. A client that falls past its queue anyway gets a `Gap` and a fresh attach, never unbounded buffering."

   herdr-core has its own bounded queue (about 4 MiB per viewer) and never stalls the program. When credit is withheld, it discards the backlog and sends an **in-band repaint**. So holding back credit doesn't produce a Gap. The daemon instead gets a resync it can spot only by the `CAN ESC [ ! p` prefix. State stays correct, but output history is lost with no event in Combine's log. The 50 MB burst with an xterm.js client hit this 11 times. A daemon Gap happens only if the daemon credits faster than the client drains (`eager`).

4. > "No output sequence, paint-complete marker or structured screen fields are exposed"

   Confirmed. Underspecified, though:
   - The paint's *start* is marked in-band (CAN, then DECSTR).
   - Its end isn't; in practice it ends with `CSI ? 9001 l`.
   - Paints over 16 KiB span several DATA frames.
   - There's no synchronized output around the paint.

5. > "When the owner detaches or its connection drops, ownership passes to the attachment that most recently had focus, or none. With no owner, the PTY keeps its last size."

   herdr-core resets the PTY to its headless size (default 200x120) when the last active viewer leaves. That means a SIGWINCH and a redraw on every last detach, daemon restart and core restart. Combine attachments that come back after a drop should use `resumed: true`.

6. > "herdr-core answers display queries itself … M0 confirms no query reaches xterm.js unanswered."

   Seven query types reach the viewer unanswered (section 3).

7. > "Flow control | CREDIT frames; units and initial allowance are in the framed protocol, not the schema"

   Now pinned down: credit is counted in bytes, the initial allowance is 256 KiB including the paint, and the core never blocks on it.

8. > "`server.restart` saves screens and the next core adopts running programs" / "reattach after it"

   Confirmed. The attach stream ends with EOF and no CLOSE frame, and re-attaching about 1 s later works (`exact`, byte-identical paint). The stream alone can't tell a restart from a crash; the daemon has to check `events.subscribe` or `terminal.get`.

9. > "M0 passes when a fresh attach restores the screen, cursor, alternate screen and input modes"

   Underspecified. The paint restores much more than that list (scroll region, pen, charset, IRM, DECOM, DECAWM, kitty keyboard flags, modifyOtherKeys, the open link). It also loses state the list doesn't mention (saved cursor, cursor shape, soft wraps, existing links, colour changes, `?1007`). The pass criterion should name those.

10. > "**Remote binary:** non-interactive SSH often lacks Homebrew's `PATH`, so each host entry stores the absolute path to `combined`"

    Confirmed, and it bit as expected. The non-interactive ssh PATH here is `/Users/jcarreira/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin`, so a bare `m0d` gives `command not found` (rc 127) and absolute paths work. `hc` is in `~/.local/bin`, which isn't on that PATH either. So SPEC's "Remote: `hc connect` speaks the API over stdio (`ssh box hc connect`)" also needs an absolute path to `hc`.

11. > "Latency budget: keystroke echo should stay under about 30 ms on a local network; M0 measures it."

    Measured locally, including ssh to localhost (section 4). The LAN measurement still needs the command in section 8.

## 7. Decisions for the owner

1. **Size for panels that aren't the owner.** Pick one:
   - **(a)** Each Combine panel uses two herdr-core viewers: an `observe` viewer for display, which gets in-band RESIZE plus replacement paints, and an active viewer for input, focus and size. This works today (`mode: "paired"` in m0d, tested). The cost is a second core connection per panel, whose stream the daemon credits as fast as it arrives and throws away.
   - **(b)** Ask herdr-core to grant `resize_notifications` (RESIZE plus a replacement paint) to active viewers whose size differs from the PTY's.
   - **(c)** Track size from `events.subscribe` `size` events. Those arrive on another channel, so ordering against data can't be guaranteed. Not recommended.

   Either way, SPEC should say that a panel shows the PTY's size, not its own.
2. **Lost first-paint state.** For each item, either add it to herdr-core's paint or accept the loss and write it down. Suggested:
   - Ask for DECSC and DECSCUSR. They're cheap and visible: vim, shells, readline and agent TUIs use both.
   - Ask for soft-wrap flags. They affect reflow on resize and copy/paste.
   - Accept the loss of existing OSC 8 links, OSC 4/10/11 colour changes and `?1007`, unless an agent TUI needs them.
3. **Unanswered queries.** Pick one:
   - herdr-core answers or swallows DECXCPR, XTGETTCAP ("not found"), `CSI 21 t`, kitty graphics `a=q`, ENQ and DECREQTPARM;
   - or SPEC lets xterm.js answer DECXCPR and accepts the rest.

   OSC 52 reads should stay unanswered for safety.
4. **Slow clients and bursts.** Pick one:
   - Accept herdr-core's in-band resync as the slow-client path. It's simple, bounded and keeps state correct, but loses history without the daemon knowing. The daemon could detect `CAN ESC[!p` and log a `resynced` event so the UI can show "output skipped".
   - Ask herdr-core for an overflow signal (a frame kind) per viewer.

   Also decide whether bursts must keep full scrollback. With an xterm.js client they don't today.
5. **Paint end marker.** Ask herdr-core for an explicit end-of-paint frame or flag. Combine needs one to show "attaching…" until the screen is whole, and to wrap the paint in synchronized output.
6. **Width tables.** Fix Combine's xterm.js Unicode version (11, or graphemes) to match herdr-core's Ghostty, and add a contract test for it.
7. **Resize ordering for active viewers.** In m0d the client applies its own resize when the `resized` event comes back through the log, so replay puts it exactly where the live client applied it. SPEC should say so. Resizing xterm.js locally ahead of the log would make replay differ from live.

## 8. The SSH bridge

- m0d listens only on a Unix socket (mode 0600); see section 4.
- `m0d connect --listen <sock>` bridges stdio to that socket.
- It was tested both by piping to a local `m0d connect` and **over real ssh to localhost**. Relay tests passed, go/no-go cases a/b passed, and latency and the burst were measured. The command was:

```sh
ssh -T -o BatchMode=yes localhost env -u HC_TERMINAL_ID -u HC_TERMINAL_SOCKET -u HC_SCOPE \
  HC_SESSION=m0 XDG_STATE_HOME=/tmp/cm0/state XDG_CONFIG_HOME=/tmp/cm0/config XDG_RUNTIME_DIR=/tmp/cm0/run \
  /Users/jcarreira/projects/squashmerge/combine-targets/m0-spike/release/m0d connect --listen /tmp/cm0/m0d.sock
```

A version mismatch over ssh returns `{"op":"error","error":"protocol version mismatch: client 9 daemon 1"}`.

To try it from the MacBook, with the mini as the host:

**1. On the mini:**
```sh
cd ~/projects/squashmerge/combine/spike/m0 && source env.sh
mkdir -p $M0/config/herdr-core && printf 'headless = "80x24"\nnotes = false\n' > $M0/config/herdr-core/config.toml
hcm server start --json && hcm_safe
(cd m0d && cargo build --release)
hcm run --bg --no-parent --name lat -- python3 $PWD/fixtures/echo.py
$CARGO_TARGET_DIR/release/m0d serve --listen /tmp/cm0/m0d.sock --core $M0_SOCK --data /tmp/cm0/m0d-data &
```

**2. On the MacBook, check the bridge.** This should print a length-prefixed JSON hello with the host ID:
```sh
printf '\x00\x00\x00\x1b\x01{"op":"hello","version":1}' | \
  ssh -T -o BatchMode=yes mini /Users/jcarreira/projects/squashmerge/combine-targets/m0-spike/release/m0d connect --listen /tmp/cm0/m0d.sock | xxd | head
```

**3. Measure LAN keystroke latency.** This needs the spike checked out and `npm ci` in `spike/m0/client`. Run it through heavy.sh if the MacBook has the lock; otherwise on an idle machine:
```sh
cd spike/m0/client
M0_BRIDGE='ssh -T -o BatchMode=yes mini /Users/jcarreira/projects/squashmerge/combine-targets/m0-spike/release/m0d connect --listen /tmp/cm0/m0d.sock' \
  node tests/measure.mjs latency-remote lat 500
```

**4. Clean up, on the mini:**
```sh
kill %1; hcm_safe && hcm kill lat && hcm rm lat && hcm server stop --json && rm -rf /tmp/cm0
```

## 9. Remaining work

- A visual xterm.js client; only headless clients were used.
- Latency over ssh on the LAN (section 8).
- The `show_diagram` half of M0, and layout restore across a restart.
- The core-crash path (`inexact`).
- htop. An agent TUI (Claude Code) as a fixture: DECSC, cursor shape, bracketed paste, synchronized output.
- The input-ownership generation protocol. m0d relies on herdr-core's arbitration and doesn't implement it.
- Large `history` attaches: they're slow and capped, and not characterised.

## Spike notes

- **Sizes:** `hc run` has no size flag; `terminal.run` takes `size`. The isolated core used `headless = "80x24"` in its config.toml. I found that key in the binary; the format is `COLSxROWS`.
- **Socket override:** `HC_SOCKET` overrides `hc`'s socket path. That's how `hc attach` was proxied.
- **Client bugs found, which the real client must avoid:**
  1. Bind the attachment id synchronously when the attach reply is parsed. Events for it can arrive in the same read.
  2. Before resuming, finish applying what already arrived on the old connection, then take the cursor. Otherwise you get duplicates, or a Gap after acking past your own cursor.
- **Comparator detail:** xterm.js stores the saved cursor as an absolute row, so the comparison makes it viewport-relative.
- **Architecture checks:** `trellis rules check` and `python3 -I tools/arch-guard/check.py` are clean with `spike/` present. Trellis indexes `spike/m0/client/package.json` as external npm packages, which is harmless noise.
