# Combine design mockups

Static mockups of the Combine window in the SquashMerge brand (brand guide v1.1). They are a visual target, not a spec: `SPEC.md` stays the source of truth for behaviour.

| File | Shows |
| --- | --- |
| `window-dev-tools.html` / `.png` | The window with the Dev tools panel set: Jira issues, pull requests and a diff |
| `window-files.html` / `.png` | The Files panel set: directory tree with changed files marked, and a file with its changes highlighted |
| `window-trellis.html` / `.png` | The Trellis panel set: apps, ordered layers, modules coloured by complexity band, and deviations |
| `tokens.html` / `.png` | Colour roles, type and the main components |

The HTML files are self-contained (fonts load from Google Fonts) and every colour, size and spacing value is an inline style. The PNGs are renders of the same files at 1440 px wide (tokens at 960 px).

## What to know before building from these

- **Panel sets are not in SPEC.md yet.** The tabs in the header (Dev tools, Files, Trellis) switch the set of panels shown beside the agent terminal. SPEC.md describes tiles and tabs within tiles, not these top-level sets.
- **All data is sample data.** The Jira keys, the diff, the file contents and every Trellis number are invented. Layer names come from `.trellis/rules.toml`.
- **Success green (`#5FD6A0`) and danger red (`#FF8A80`) are proposals.** The brand guide has no tokens for them.
- **Colour is never the only cue.** States pair a colour with a word and a shape; changed files carry an M or A badge; diff lines carry a + or − sign.

The logo files are in `docs/brand/`.
