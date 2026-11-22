export const TERMINAL_THEME = {
  foreground: '#d4d4d4',
  background: '#1e1e1e',
  cursor: '#d4d4d4',
} as const;

export const TERMINAL_FONT = {
  family: 'Menlo, Monaco, monospace',
  size: 13,
} as const;

/** Used for xterm's `scrollback` and for `attach.options.history`. */
export const TERMINAL_HISTORY_LINES = 1000;

export const LETTERBOX_BACKGROUND = '#141414';
