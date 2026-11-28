import { COLOR, FONT_FAMILY } from '../../model/tokens';

export const TERMINAL_THEME = {
  foreground: COLOR.text,
  background: COLOR.ink,
  cursor: COLOR.amber,
  cursorAccent: COLOR.ink,
  selectionBackground: COLOR.selection,
} as const;

export const TERMINAL_FONT = {
  family: FONT_FAMILY.mono,
  size: 13,
} as const;

/** Used for xterm's `scrollback` and for `attach.options.history`. */
export const TERMINAL_HISTORY_LINES = 1000;

export const LETTERBOX_BACKGROUND = COLOR.surface;
