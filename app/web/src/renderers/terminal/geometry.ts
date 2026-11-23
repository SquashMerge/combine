import type { Display, TerminalSize } from '../../model/terminal';
import type { CellSize } from './adapter';
import type { TERMINAL_THEME } from './settings';

const MAX_DIMENSION = 65535;

/** Used until an instance has measured the real cell. */
export const DEFAULT_CELL: CellSize = { width: 8, height: 17 };
export const FALLBACK_SIZE: TerminalSize = { cols: 80, rows: 24 };

/**
 * Floors by the unrounded measured cell, so the grid never overflows the container.
 * Null when either dimension is below one cell or the cell is not positive.
 */
export function fitGrid(
  container: { width: number; height: number },
  cell: CellSize,
): TerminalSize | null {
  if (!(cell.width > 0) || !(cell.height > 0)) return null;
  const cols = Math.min(Math.floor(container.width / cell.width), MAX_DIMENSION);
  const rows = Math.min(Math.floor(container.height / cell.height), MAX_DIMENSION);
  if (!(cols >= 1) || !(rows >= 1)) return null;
  return { cols, rows };
}

function wholePixels(value: number): number {
  return Math.min(Math.max(Math.round(value), 1), MAX_DIMENSION);
}

export function displayFrom(theme: typeof TERMINAL_THEME, cell: CellSize): Display {
  return {
    foreground: theme.foreground,
    background: theme.background,
    cursor_color: theme.cursor,
    cell_width: wholePixels(cell.width),
    cell_height: wholePixels(cell.height),
  };
}

export function viewportPixels(
  size: TerminalSize,
  cell: CellSize,
): { width: number; height: number } {
  return { width: size.cols * cell.width, height: size.rows * cell.height };
}
