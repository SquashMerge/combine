# herdr-core requests from Combine

Drafted 2026-11-20 from Combine's M0 terminal-fidelity spike (`spike/m0/FINDINGS.md`), against herdr-core 0.1.21, protocol generation 1. Combine attaches to herdr-core terminals over the framed protocol and relays each attachment's stream, unparsed, to xterm.js clients that may reconnect at any time. Both requests are about what a viewer can know after a fresh attach or a slow patch.

Combine works without either change and records both gaps as known limitations (SPEC.md, "Where the first paint comes from"). Neither request changes behavior for viewers that don't ask for it.

Target: [herdrdev/herdr-core](https://github.com/herdrdev/herdr-core) issues, as its CONTRIBUTING.md asks. Filed 2026-11-20 as #354 and #355; neither was already filed or fixed (0.1.21, plus the Unreleased changelog). Related open issues: #233 (attach has no resume position), #232 (the paint isn't one synchronized update), #91 (a new viewer's repaint drops OSC 4/10/11/12 colours), #69 (two viewers take the size from each other). The framed protocol M0 relied on is documented in herdr-core's `schema/README.md`, "Framed mode"; the second request extends what it says about falling 4 MiB behind.

## 1. Include the DECSC saved cursor in the first paint

Filed as [herdrdev/herdr-core#354](https://github.com/herdrdev/herdr-core/issues/354).

**Today.** A program saves the cursor with DECSC (`ESC 7`, or `CSI s`), draws elsewhere (a status line, a progress bar), then restores it with DECRC (`ESC 8`). After a fresh attach, the viewer's emulator has no saved cursor, so the next DECRC moves to (0,0). The program's following output overwrites the top-left of the screen until it next redraws everything.

**Evidence.** `spike/m0/fixtures/modes.py` saves the cursor at row 15, column 5 on the alternate screen and idles. After any fresh attach (before the daemon existed, after a daemon restart, after `hc server restart`), sending `r` makes it write `ESC 8` then "RC". The reference emulator, which saw the whole stream, puts "RC" at (14,4); the fresh attach puts it at (0,0), over the header. Results: `spike/m0/results/gonogo-modes.json`, case `live_r_saved_cursor_DECRC_`. vim, readline and agent TUIs use DECSC routinely.

**Request.** The first paint (and every replacement and resync paint) restores the saved-cursor state of each screen buffer, as DECSC saves it:
- position;
- SGR rendition;
- character set designations (G0–G3) and shift state;
- origin mode;
- the pending-wrap flag where the emulator tracks one.

**Constraint on the paint's order:** DECSC saves the *current* pen and charsets. So the paint has to set the saved values, issue `ESC 7`, then set the live cursor position, pen and charsets. That applies to both the main and the alternate screen.

**Acceptance:**
1. With `modes.py`, the `r` check after a fresh attach matches the reference in all three attach cases (fresh, daemon restart, `server.restart`).
2. A saved cursor on the main screen, made before entering the alternate screen, is restored after leaving it.
3. No saved cursor (never set) paints nothing extra, so a later DECRC behaves as on a fresh terminal.

## 2. Tell a viewer when its backlog was dropped

Filed as [herdrdev/herdr-core#355](https://github.com/herdrdev/herdr-core/issues/355).

**Today.** Credit never stalls the program. When a viewer stops granting credit, herdr-core queues about 4 MiB for it, then drops the queue and sends a fresh paint in-band (`CAN ESC [ ! p ...`). The screen ends right, which is the right call, but the viewer can't tell that output was skipped:
- matching `CAN ESC [ ! p` in DATA is a guess, since program output can contain those bytes and frames can split them;
- nothing else in the stream marks it.

Combine wants to show "output skipped" on the panel and keep its own replay log honest about missing history.

**Evidence.** `spike/m0/tools/probes/exp_stall.py 20 4`: a 20 MB burst while the viewer withholds credit for 4 s. After crediting, the viewer got lines 0..4091, then a full paint whose screen began at line 266169, then live output to the end. Lines 4092..266168 were never sent, and no frame said so. A 50 MB burst into an xterm.js client hit this 11 times (`spike/m0/results/burst.json`).

**Request.** A frame on the viewer's channel, immediately before the replacement paint it introduces, saying that output was dropped:
- **new kind**, for example 7 RESYNC;
- **payload** is JSON, at least `{"dropped_bytes": <u64>}`;
- **opt-in** with a `hello` feature (for example `resync_notifications`), so existing viewers see no change;
- **ordered**: on the same channel and in order with DATA, so a relay that never parses DATA can log it at the right place.

**Acceptance:**
1. With the feature on, `exp_stall.py 20 4` sees exactly one RESYNC, immediately before the `CAN ESC [ ! p` paint, with `dropped_bytes` equal to the bytes not delivered.
2. With it off, the stream is byte-identical to 0.1.21's.
3. No RESYNC when the viewer keeps up (a byte-counting client on a 50 MB burst).

## Not requested

Combine accepts the rest of what M0 found and works around one item:
- **Accepted as known limitations:** the first paint doesn't carry the cursor shape, soft-wrap flags, OSC 8 links on cells already written, or palette and default-colour changes; its end isn't marked; and seven queries go unanswered (DECXCPR, `CSI 21 t`, XTGETTCAP, OSC 52 read, ENQ, DECREQTPARM, kitty graphics `a=q`).
- **Worked around:** resize notifications go only to `observe` viewers, so Combine pairs an observe viewer with an active one for each panel. If herdr-core later grants ordered RESIZE plus a replacement paint to active viewers too, Combine can use one viewer per panel; that would be welcome, but it isn't needed.
