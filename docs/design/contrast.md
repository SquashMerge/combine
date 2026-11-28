# Contrast (WCAG 2.x AA)

Ratios come from the colours in `app/web/src/model/tokens.ts` and are checked by `app/web/test/contrast.test.ts`; "at 0.85" means the colour composited at 0.85 opacity over `--ink` (the held layout).

## Pairs in use

| Kind | Foreground | Background | Ratio | Minimum |
| --- | --- | --- | --- | --- |
| text | `--text` | `--ink` | 18.29 | 4.5 |
| text | `--text` | `--surface` | 17.02 | 4.5 |
| text | `--text` | `--raised` | 15.46 | 4.5 |
| text | `--text-muted` | `--ink` | 10.14 | 4.5 |
| text | `--text-muted` | `--surface` | 9.44 | 4.5 |
| text | `--text-muted` | `--raised` | 8.57 | 4.5 |
| text | `--text-muted at 0.85` | `--surface at 0.85` | 7.08 | 4.5 |
| text | `--ink` | `--amber` | 10.25 | 4.5 |
| text | `--ink` | `--purple` | 4.78 | 4.5 |
| text | `--danger` | `--surface` | 7.97 | 4.5 |
| text | `--danger` | `--raised` | 7.24 | 4.5 |
| text | `--danger` | `--ink` | 8.56 | 4.5 |
| text | `--lavender` | `--surface` | 8.28 | 4.5 |
| ui | `--control-border` | `--surface` | 4.29 | 3 |
| ui | `--control-border` | `--raised` | 3.90 | 3 |
| ui | `--control-border` | `--ink` | 4.62 | 3 |
| ui | `--purple` | `--surface` | 4.45 | 3 |
| ui | `--purple` | `--ink` | 4.78 | 3 |
| ui | `--purple` | `--raised` | 4.04 | 3 |
| ui | `--lavender` | `--surface` | 8.28 | 3 |
| ui | `--lavender` | `--raised` | 7.52 | 3 |
| ui | `--lavender` | `--ink` | 8.90 | 3 |
| ui | `--amber` | `--surface` | 9.53 | 3 |
| ui | `--amber` | `--raised` | 8.66 | 3 |
| ui | `--success` | `--surface` | 10.06 | 3 |

## Pairs never used

| Kind | Foreground | Background | Ratio | Minimum |
| --- | --- | --- | --- | --- |
| text | `--text` | `--purple` | 3.83 | 4.5 |
| text | `--text-muted` | `--purple` | 2.12 | 4.5 |
| text | `--control-border` | `--surface` | 4.29 | 4.5 |
| text | `--control-border` | `--raised` | 3.90 | 4.5 |
| ui | `--lavender` | `--purple` | 1.86 | 3 |
| ui | `--lavender` | `--amber` | 1.15 | 3 |

## Exempt

- Disabled controls (`:disabled` at `--disabled-opacity`) are exempt under WCAG 2.x.
- The stale screen at opacity 0.4 is inactive and sits under the overlay.
- `--raised` dividers and resting tile borders (1.10) are decorative and carry no meaning.
