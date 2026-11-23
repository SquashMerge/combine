import type { TerminalSize } from '../../model/terminal';

/** CSS pixels; may be fractional. */
export interface CellSize {
  width: number;
  height: number;
}

export interface XtermCallbacks {
  /** Keys, paste and mouse (xterm's onData and onBinary). */
  onInput(bytes: Uint8Array): void;
  /** DECXCPR replies only. */
  onAutomaticReply(bytes: Uint8Array): void;
  /** Mode-1004 focus in and out reports (CSI I, CSI O). */
  onFocusReport(bytes: Uint8Array): void;
  /** DOM focus entered the instance. */
  onFocus(): void;
  /** The measured cell changed; never zero. */
  onCellSize(cell: CellSize): void;
}

/** There is no clear method: the screen is never cleared, by structure. */
export interface XtermAdapter {
  readonly renderer: 'webgl' | 'dom';
  /** `done` runs in xterm's write callback, after parsing. */
  write(bytes: Uint8Array, done: () => void): void;
  resize(size: TerminalSize): void;
  focus(): void;
  hasFocus(): boolean;
  /** Disposes the WebGL addon first, then Unicode11, then the terminal, then its element. */
  dispose(): void;
}

export type XtermAdapterFactory = (
  host: HTMLElement,
  size: TerminalSize,
  callbacks: XtermCallbacks,
) => XtermAdapter;
