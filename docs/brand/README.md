# Combine logo files

The Combine mark is two branching stems side by side: a warm (amber) one on the left and a cool (lavender) one on the right, each with ring-tipped branches. Every file is a hand-written SVG.

| File | Use it for |
| --- | --- |
| `combine-mark.svg` | The full mark in colour: three branches a side, taller than wide. Headers, about screens, docs. |
| `combine-mark-ink.svg` | The same mark in one dark ink (`#0B0C10`), for light backgrounds or where colour can't be used. |
| `combine-mark-white.svg` | The same mark in white (`#F7F7FB`), for dark backgrounds. |
| `combine-icon.svg` | The compact mark in colour: two branches a side, nearly square, no background. In-app logo, favicons, small spaces. |
| `combine-icon-small.svg` | `combine-icon.svg` for small sizes: solid dots instead of rings and heavier strokes. |
| `combine-app-icon.svg` | The macOS app icon, 1024 × 1024: the compact mark on a dark rounded tile (`#1D1E2A`), inset to Apple's icon grid. |
| `combine-app-icon-small.svg` | `combine-app-icon.svg` with the small-size mark. |

## Colours

Both gradients run top to bottom.

| Gradient | Top | Bottom |
| --- | --- | --- |
| Warm (left stem) | `#FFC20A` | `#FF7A00` |
| Cool (right stem) | `#ABA5FF` | `#5D55C8` |

## Small sizes

At 32 px and below the ring holes close up and the mark turns muddy. Use a `-small` file there: its branches end in solid dots. The app icon's `.icns` already does: its 16 px and 32 px slots, and their @2x, come from `combine-app-icon-small.svg`, and the 128 px slots and up come from `combine-app-icon.svg`.

## Gradient IDs

Each file's gradient IDs start with its own prefix (`cm`, `ci`, `cs`, `ca`, `ct`, then `w` for warm and `c` for cool), so several of these SVGs can be inlined in one HTML page without their gradients clashing. Keep the prefixes unique when adding a file.

## The app icon

`app/src-tauri/icons/icon.png` (1024 px) and `icon.icns` are the two files `app/src-tauri/tauri.conf.json` lists. `icon.png` and the `.icns` slots from 128 px up are renders of `combine-app-icon.svg`; the 16 px and 32 px slots (and @2x) are renders of `combine-app-icon-small.svg`. To rebuild the `.icns`, render each slot into an `icon.iconset` folder (`icon_16x16.png`, `icon_16x16@2x.png`, … `icon_512x512@2x.png`) and run `iconutil -c icns icon.iconset`. `app/web/node_modules/.bin/tauri icon <svg>` renders one SVG at every size, so it can't mix the two files by itself.
