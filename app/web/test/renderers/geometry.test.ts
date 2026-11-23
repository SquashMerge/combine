import { describe, expect, it } from 'vitest';
import {
  DEFAULT_CELL,
  FALLBACK_SIZE,
  displayFrom,
  fitGrid,
  viewportPixels,
} from '../../src/renderers/terminal/geometry';
import { TERMINAL_THEME } from '../../src/renderers/terminal/settings';

describe('terminal geometry', () => {
  it('has the documented defaults', () => {
    expect(DEFAULT_CELL).toEqual({ width: 8, height: 17 });
    expect(FALLBACK_SIZE).toEqual({ cols: 80, rows: 24 });
  });

  it('fitGrid floors the container by the cell size', () => {
    expect(fitGrid({ width: 805, height: 410 }, { width: 8, height: 17 })).toEqual({
      cols: 100,
      rows: 24,
    });
    expect(fitGrid({ width: 100, height: 100 }, { width: 7.9, height: 16.5 })).toEqual({
      cols: 12,
      rows: 6,
    });
    expect(fitGrid({ width: 100, height: 100 }, { width: 7.4, height: 16.6 })).toEqual({
      cols: 13,
      rows: 6,
    });
    expect(fitGrid({ width: 8, height: 17 }, DEFAULT_CELL)).toEqual({ cols: 1, rows: 1 });
    expect(fitGrid({ width: 1e9, height: 1e9 }, { width: 1, height: 1 })).toEqual({
      cols: 65535,
      rows: 65535,
    });
  });

  it('fitGrid returns null below one cell', () => {
    expect(fitGrid({ width: 7, height: 100 }, DEFAULT_CELL)).toBeNull();
    expect(fitGrid({ width: 100, height: 16 }, DEFAULT_CELL)).toBeNull();
    expect(fitGrid({ width: 100, height: 100 }, { width: 0, height: 17 })).toBeNull();
    expect(fitGrid({ width: 100, height: 100 }, { width: 8, height: -1 })).toBeNull();
    expect(fitGrid({ width: 100, height: 100 }, { width: Number.NaN, height: 17 })).toBeNull();
  });

  it('displayFrom rounds the cell size to whole pixels and uses the theme colours', () => {
    expect(displayFrom(TERMINAL_THEME, { width: 7.6, height: 16.4 })).toEqual({
      foreground: '#d4d4d4',
      background: '#1e1e1e',
      cursor_color: '#d4d4d4',
      cell_width: 8,
      cell_height: 16,
    });
    const tiny = displayFrom(TERMINAL_THEME, { width: 0.2, height: 0.4 });
    expect([tiny.cell_width, tiny.cell_height]).toEqual([1, 1]);
    const huge = displayFrom(TERMINAL_THEME, { width: 1e9, height: 70000 });
    expect([huge.cell_width, huge.cell_height]).toEqual([65535, 65535]);
  });

  it('viewportPixels is the grid times the cell', () => {
    expect(viewportPixels({ cols: 80, rows: 24 }, { width: 7.5, height: 17 })).toEqual({
      width: 600,
      height: 408,
    });
  });
});
