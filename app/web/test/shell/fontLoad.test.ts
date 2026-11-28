import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  FONT_LOAD_SPECS,
  FONT_LOAD_TIMEOUT_MS,
  renderAfterFonts,
  waitForFonts,
} from '../../src/shell/fontLoad';

interface Deferred {
  resolve: (faces: FontFace[]) => void;
  reject: (error: Error) => void;
}

function fakeFonts(check = true) {
  const pending: Deferred[] = [];
  const load = vi.fn(
    (_spec: string) =>
      new Promise<FontFace[]>((resolve, reject) => {
        pending.push({ resolve, reject });
      }),
  );
  const fonts = { load, check: vi.fn((_spec: string) => check) };
  const settleAll = (faces: FontFace[]) => pending.forEach((entry) => entry.resolve(faces));
  return { fonts, load, pending, settleAll };
}

const face = [{} as FontFace];

describe('fontLoad', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('names the five faces, the terminal mono first', () => {
    expect(FONT_LOAD_SPECS).toEqual([
      "400 13px 'JetBrains Mono'",
      "700 13px 'JetBrains Mono'",
      "400 14px 'Inter'",
      "600 14px 'Inter'",
      "700 18px 'Manrope'",
    ]);
    expect(FONT_LOAD_SPECS[0]).toBe("400 13px 'JetBrains Mono'");
  });

  it('is unsupported without a font set', async () => {
    expect(await waitForFonts(undefined)).toBe('unsupported');
  });

  it('asks to load every spec in order, and reports loaded', async () => {
    const { fonts, load, settleAll } = fakeFonts();
    const outcome = waitForFonts(fonts);
    expect(load.mock.calls.map(([spec]) => spec)).toEqual([...FONT_LOAD_SPECS]);
    settleAll(face);
    expect(await outcome).toBe('loaded');
    expect(vi.getTimerCount()).toBe(0);
  });

  it('is missing when a load returns no face', async () => {
    const { fonts, settleAll } = fakeFonts();
    const outcome = waitForFonts(fonts);
    settleAll([]);
    expect(await outcome).toBe('missing');
  });

  it('is missing when check says a face is absent', async () => {
    const { fonts, settleAll } = fakeFonts(false);
    const outcome = waitForFonts(fonts);
    settleAll(face);
    expect(await outcome).toBe('missing');
  });

  it('is failed when a load rejects', async () => {
    const { fonts, pending, settleAll } = fakeFonts();
    const outcome = waitForFonts(fonts);
    pending[0]?.reject(new Error('network'));
    settleAll(face);
    expect(await outcome).toBe('failed');
  });

  it('times out when the loads never settle', async () => {
    const { fonts } = fakeFonts();
    const outcome = waitForFonts(fonts);
    await vi.advanceTimersByTimeAsync(FONT_LOAD_TIMEOUT_MS);
    expect(await outcome).toBe('timeout');
    expect(FONT_LOAD_TIMEOUT_MS).toBe(3000);
  });

  it('renders once after the loads settle, and not again on a late settle', async () => {
    const { fonts, settleAll } = fakeFonts();
    const render = vi.fn();
    const done = renderAfterFonts(fonts, render, 100);
    await Promise.resolve();
    expect(render).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(100);
    expect(await done).toBe('timeout');
    expect(render).toHaveBeenCalledTimes(1);
    settleAll(face);
    await vi.runAllTimersAsync();
    expect(render).toHaveBeenCalledTimes(1);
  });

  it('renders on every outcome', async () => {
    const render = vi.fn();
    expect(await renderAfterFonts(undefined, render)).toBe('unsupported');
    const { fonts, settleAll } = fakeFonts();
    const done = renderAfterFonts(fonts, render);
    settleAll(face);
    expect(await done).toBe('loaded');
    expect(render).toHaveBeenCalledTimes(2);
  });
});
