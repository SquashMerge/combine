import { describe, expect, it } from 'vitest';
import { BUNDLED_FONT, COLOR, FONT_FAMILY } from '../src/model/tokens';
import {
  LETTERBOX_BACKGROUND,
  TERMINAL_FONT,
  TERMINAL_THEME,
} from '../src/renderers/terminal/settings';
import { parseRules, read, srcRoot } from './support/css';

const tokensCss = read(`${srcRoot}/shell/tokens.css`);
const root = parseRules(tokensCss);
const kebab = (key: string): string => key.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);

describe('tokens', () => {
  it('holds the brand colours', () => {
    expect(COLOR).toEqual({
      ink: '#0b0c10',
      surface: '#14151d',
      raised: '#1d1e2a',
      purple: '#7770dc',
      lavender: '#aba5ff',
      amber: '#ffaa10',
      text: '#f7f7fb',
      textMuted: '#b8bac8',
      controlBorder: '#777a8e',
      success: '#5fd6a0',
      danger: '#ff8a80',
      selection: '#aba5ff4d',
      scrim: '#0b0c1099',
    });
    for (const value of Object.values(COLOR)) expect(value).toBe(value.toLowerCase());
  });

  it('derives selection and scrim from lavender and ink', () => {
    expect(COLOR.selection).toBe(`${COLOR.lavender}4d`);
    expect(COLOR.scrim).toBe(`${COLOR.ink}99`);
  });

  it('names the bundled fonts and their families', () => {
    expect(BUNDLED_FONT).toEqual({ display: 'Manrope', ui: 'Inter', mono: 'JetBrains Mono' });
    expect(FONT_FAMILY).toEqual({
      display: "'Manrope', system-ui, sans-serif",
      ui: "'Inter', system-ui, sans-serif",
      mono: "'JetBrains Mono', Menlo, monospace",
    });
  });

  it('is one :root block that repeats every token byte for byte', () => {
    expect(root).toHaveLength(1);
    expect(root[0]?.selector).toBe(':root');
    const declarations = root[0]?.declarations ?? {};
    const expected: Record<string, string> = { '--disabled-opacity': '0.45' };
    for (const [key, value] of Object.entries(COLOR)) expected[`--${kebab(key)}`] = value;
    for (const [key, value] of Object.entries(FONT_FAMILY)) expected[`--font-${key}`] = value;
    expect(declarations).toEqual(expected);
    expect(tokensCss).not.toContain('color-mix');
  });

  it('holds no colour literal that is not a COLOR value', () => {
    const literals = tokensCss.match(/#[0-9a-fA-F]{3,8}\b/g) ?? [];
    const known = new Set<string>(Object.values(COLOR));
    expect(literals.filter((literal) => !known.has(literal))).toEqual([]);
  });

  it('takes the terminal theme, letterbox and font from the tokens', () => {
    expect(TERMINAL_THEME).toEqual({
      foreground: COLOR.text,
      background: COLOR.ink,
      cursor: COLOR.amber,
      cursorAccent: COLOR.ink,
      selectionBackground: COLOR.selection,
    });
    expect(LETTERBOX_BACKGROUND).toBe(COLOR.surface);
    expect(TERMINAL_FONT).toEqual({ family: FONT_FAMILY.mono, size: 13 });
  });
});
