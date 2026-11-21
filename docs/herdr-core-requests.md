# herdr-core requests from Combine

Drafted 2026-11-20 from Combine's M0 terminal-fidelity spike (`spike/m0/FINDINGS.md`), against herdr-core 0.1.21, protocol generation 1. Combine attaches to herdr-core terminals over the framed protocol and relays each attachment's stream, unparsed, to xterm.js clients that may reconnect at any time. Both requests are about what a viewer can know after a fresh attach or a slow patch.

Combine works without either change and records both gaps as known limitations (SPEC.md, "Where the first paint comes from"). The resync signal is opt-in; the saved-cursor fix changes paint bytes for every viewer, as a correctness fix.

Target: [herdrdev/herdr-core](https://github.com/herdrdev/herdr-core) issues, as its CONTRIBUTING.md asks. Filed 2026-11-20 as #354 and #355; neither was already filed or fixed (0.1.21, plus the Unreleased changelog). Related open issues: #233 (attach has no resume position), #232 (the paint isn't one synchronized update), #91 (a new viewer's repaint drops OSC 4/10/11/12 colours), #69 (two viewers take the size from each other). The framed protocol M0 relied on is documented in herdr-core's `schema/README.md`, "Framed mode"; the second request extends what it says about falling 4 MiB behind.

## 1. Include the saved cursor in every paint

Filed as [herdrdev/herdr-core#354](https://github.com/herdrdev/herdr-core/issues/354); the issue text, revised after review 5:

A viewer that attaches fresh has no saved cursor, so a program's next DECRC (`ESC 8` / `CSI u`) goes to (0,0) and its following output overwrites the top-left of the screen until the program redraws everything.

### What happens

A program saves the cursor with DECSC (`ESC 7` or `CSI s`), draws somewhere else (a status line, a progress bar), then restores it with DECRC. vim, readline and agent TUIs do this routinely. The first paint (and every replacement and resync paint) restores the screen, the cursor, the pen, modes and the scroll region, but not the saved-cursor state. So after a reattach, a restart on the viewer's side, or `hc server restart`, the viewer has no saved cursor, and DECRC lands at (0,0).

### Reproduction (herdr-core 0.1.21)

From [Combine](https://github.com/SquashMerge/combine)'s terminal-fidelity spike. Run from `spike/m0` after `source env.sh`, which sets up an isolated core (`HC_SESSION=m0`, scratch XDG paths); `hcm_safe` checks it isn't the default core.

1. [`fixtures/modes.py`](https://github.com/SquashMerge/combine/blob/65167f0d6a64dc85f8a6261d3e6581bea16171e0/spike/m0/fixtures/modes.py) enters the alternate screen, saves the cursor at row 15, column 5 with DECSC, moves elsewhere, and idles.
2. Attach a fresh viewer (any of: before any viewer existed, after the viewer restarted, after `hc server restart`).
3. Send `r`: the fixture writes `ESC 8`, then "RC".

A reference emulator that saw the whole stream puts "RC" at (14,4) (zero-based); the fresh viewer puts it at (0,0), over the header. Results: [`gonogo-modes.json`](https://github.com/SquashMerge/combine/blob/65167f0d6a64dc85f8a6261d3e6581bea16171e0/spike/m0/results/gonogo-modes.json), case `live_r_saved_cursor_DECRC_`; write-up in [FINDINGS.md](https://github.com/SquashMerge/combine/blob/65167f0d6a64dc85f8a6261d3e6581bea16171e0/spike/m0/FINDINGS.md), section 2.

### Request

After a paint, **both** buffers' saved-cursor registers and the live state should match the terminal's. That covers:
- position;
- rendition;
- character set designations and shift state;
- the pending-wrap flag where tracked.

Two things make this more than "add `ESC 7`":

- **Buffer switches save too.** `CSI ? 1049 h`, which today's paint uses to enter the alternate screen, saves the main buffer's cursor itself. A serializer that establishes the main buffer's saved state with `ESC 7` and then switches with `?1049h` overwrites it. The main buffer's saved state has to survive the switch, for example by letting `?1049h` do the save from the intended saved values, or by switching with a mode that doesn't save. A terminal entered through a non-saving switch can have different saved and live main-screen state, so this matters.
- **Order within a buffer.** DECSC saves the *current* pen and charsets, so set the saved values, save, then set the live position, pen and charsets. Set margins and origin mode first, so restoring the live state doesn't home or clamp the cursor afterwards.

**Receiver limits.** Receivers differ in what DECSC saves. xterm.js saves position, the basic attributes and the charset, but not origin mode or extended attributes such as underline style and colour. A paint can only reconstruct what the receiver's own DECSC stores, so position, basic rendition and charsets are the useful core. The rest is best effort.

**Compatibility.** This changes ordinary paint bytes for every viewer, with no opt-in. It's a correctness fix to the paint, in the same category as restoring the scroll region or the pen.

### Acceptance

1. With `modes.py`, the DECRC check after a fresh attach matches the reference in all three attach cases.
2. **Distinct state on both screens:** different saved and live positions, pens and charsets on both buffers, and a saving and a non-saving alternate-screen switch. After a fresh, a replacement and a resync paint into an already-used receiver, DECRC on each buffer restores the same state as that same receiver fed the original complete stream, for the fields that receiver's DECSC saves. Saved origin mode and extended rendition are checked only on receivers that implement saving them.
3. A saved cursor on the main screen, made before entering the alternate screen, is restored after leaving it.
4. With no saved cursor ever set, the paint adds nothing, so a later DECRC behaves as on a fresh terminal.

Related: #91 (colours set with OSC 4/10/11/12 are also missing from a new viewer's repaint).

## 2. Tell a viewer when undelivered output was replaced by a repaint

Filed as [herdrdev/herdr-core#355](https://github.com/herdrdev/herdr-core/issues/355); the issue text, revised after review 5:

herdr-core sometimes replaces output a viewer hasn't received yet with a fresh paint. The screen ends up right, but the viewer can't tell that output was skipped. Could the core say so, in order on the viewer's channel?

### When output is superseded

`schema/README.md` ("Framed mode") describes two cases:

1. **A viewer falls more than 4 MiB behind:** it "gets a fresh repaint instead of the backlog".
2. **A confirmed observer's grid changes:** "pending output is superseded by RESIZE followed by an authoritative paint".

Either way, bytes the viewer never received are gone. Today the only clue is the `CAN ESC [ ! p` at the start of the paint. That's a guess: program output can contain those bytes, and frames can split them.

### Why it matters

[Combine](https://github.com/SquashMerge/combine) relays each attachment's stream to remote xterm.js clients without parsing it, and keeps a replay log so clients can reconnect. It wants to:
- show "output skipped" on a panel;
- keep its log honest that history has a hole there.

### Reproduction of case 1 (herdr-core 0.1.21)

Run from `spike/m0` after `source env.sh`, which sets up an isolated core (`HC_SESSION=m0`, scratch XDG paths); `hcm_safe` checks it isn't the default core.

```sh
python3 -I tools/probes/exp_stall.py 20 4 req-resync
```

[`exp_stall.py`](https://github.com/SquashMerge/combine/blob/65167f0d6a64dc85f8a6261d3e6581bea16171e0/spike/m0/tools/probes/exp_stall.py) (arguments: MB, seconds, terminal name) sends a 20 MB burst while the viewer reads but withholds credit for 4 s.
- After crediting, the viewer got lines 0..4091, then a full paint whose screen began at line 266169, then live output to the end.
- Lines 4092..266168 were never sent, and no frame said so.

The probe prints every non-DATA frame; testing the request needs a variant that asserts on the new frame. A 50 MB burst into a client that applies every byte to xterm.js hit this 11 times. Details: [FINDINGS.md](https://github.com/SquashMerge/combine/blob/65167f0d6a64dc85f8a6261d3e6581bea16171e0/spike/m0/FINDINGS.md), sections 1 and 4.

### Request

A RESYNC frame (a new kind, for example 7) on the viewer's channel, **immediately before the first byte of the paint that replaces superseded output**.

**Payload.** JSON such as `{"reason": "behind" | "resize", "dropped_bytes": <u64 or null>}`.
- `dropped_bytes` counts live output bytes, after the core's query filtering, that were queued for this viewer and never sent.
- `null` means "history was lost, count unknown", if an exact count isn't practical. The marker matters more than the number.

**Opt-in.** The viewer asks with `resync_notifications: true` on `terminal.attach`, and gets the frame only if the reply confirms it, as with `resize_notifications`. The core advertises it in `hello`.

**Coalescing.** The frame goes with the paint as one unit, sent when the paint starts, not when the loss is first detected.
- More output superseded while that paint still waits for credit is added to the same pending RESYNC, so there's at most one pending per viewer.
- If a paint that has already started is superseded, a new RESYNC precedes its replacement.
- The paint keeps the authoritative RESIZE boundary, including any resize in between.

**Credit.** RESYNC doesn't consume DATA credit, like RESIZE, and its pending state is bounded by the coalescing above.

### Acceptance

1. With the feature confirmed, the probe sees exactly one RESYNC immediately before the `CAN ESC[!p` paint. With an exact count, `dropped_bytes` equals the bytes not delivered.
2. **Edge cases:**
   - repeated overflow with zero or tiny credit (one RESYNC per paint actually sent);
   - a paint split across frames;
   - a resize while stalled;
   - a resize for a confirmed observer with output still pending (`reason: "resize"`);
   - a program that writes `CAN ESC [ ! p` itself (no RESYNC).
3. With the feature off, there are no new frames, and DATA is identical to the same server's stream without this extension.
4. No RESYNC when the viewer keeps up (a byte-counting client on a 50 MB burst) and no grid change happens.

Related: #233 (attach has no resume position), #232 (the paint isn't one synchronized update).

## Not requested

Combine accepts the rest of what M0 found and works around one item:
- **Accepted as known limitations:** the first paint doesn't carry the cursor shape, soft-wrap flags, OSC 8 links on cells already written, or palette and default-colour changes; its end isn't marked; and seven queries go unanswered (DECXCPR, `CSI 21 t`, XTGETTCAP, OSC 52 read, ENQ, DECREQTPARM, kitty graphics `a=q`).
- **Worked around:** resize notifications go only to `observe` viewers, so Combine pairs an observe viewer with an active one for each panel. If herdr-core later grants ordered RESIZE plus a replacement paint to active viewers too, Combine can use one viewer per panel; that would be welcome, but it isn't needed.
