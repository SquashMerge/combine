import { describe, expect, it } from 'vitest';
import { COLOR } from '../src/model/tokens';
import { FONT_LOAD_SPECS } from '../src/shell/fontLoad';
import { cssFiles, parseRules, read, relativeToWeb, srcRoot, type CssRule } from './support/css';

// Cross-slice acceptance checks: slice A (tokens, fonts, font load) against slice B (components, glyphs, rail logo).

const rules: (CssRule & { file: string })[] = cssFiles().flatMap((path) =>
  parseRules(read(path)).map((rule) => ({ ...rule, file: relativeToWeb(path) })),
);
const selectorList = (rule: CssRule): string[] => rule.selector.split(',').map((part) => part.trim());

const SHAPES = ['filled-dot', 'half-ring', 'ring', 'square', 'dashed-ring', 'cross', 'check'];

function luminance(hex: string): number {
  const n = Number.parseInt(hex.slice(1, 7), 16);
  const [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((channel) => {
    const v = channel / 255;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  }) as [number, number, number];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}
const ratio = (a: string, b: string): number => {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
};

function glyphColourToken(shape: string): string[] {
  return rules
    .filter((r) => selectorList(r).some((s) => s === `.state-glyph[data-glyph="${shape}"]`))
    .map((r) => r.declarations['color'] ?? '');
}

describe('state glyphs (AC9, AC12)', () => {
  it.each(SHAPES)('colours %s with exactly one colour token that is 3:1 on the surfaces it sits on', (shape) => {
    const colours = glyphColourToken(shape);
    expect(colours, shape).toHaveLength(1);
    const match = /^var\(--([a-z-]+)\)$/.exec(colours[0] ?? '');
    expect(match, `${shape}: ${colours[0]}`).not.toBeNull();
    const key = (match?.[1] ?? '').replace(/-([a-z])/g, (_, c: string) => c.toUpperCase());
    const hex = (COLOR as Record<string, string>)[key];
    expect(hex, `${shape}: token ${key}`).toBeDefined();
    for (const background of [COLOR.surface, COLOR.raised, COLOR.ink]) {
      expect(ratio(hex ?? '', background), `${shape} on ${background}`).toBeGreaterThanOrEqual(3);
    }
  });

  it('uses the success and danger colours for the check and the cross', () => {
    expect(glyphColourToken('check')).toEqual(['var(--success)']);
    expect(glyphColourToken('cross')).toEqual(['var(--danger)']);
  });
});

describe('rail logo (AC11)', () => {
  it('sizes the small-variant logo at 32px or below beside the Combine wordmark', () => {
    const logo = rules.find((r) => selectorList(r).includes('.rail-brand img'));
    for (const property of ['width', 'height']) {
      expect(Number.parseInt(logo?.declarations[property] ?? '999', 10), property).toBeLessThanOrEqual(32);
    }
    expect(read(`${srcRoot}/shell/SpaceRail.tsx`)).toContain('combine-icon-small.svg');
  });
});

describe('bundled weights per family (AC3, AC4)', () => {
  const ALLOWED: Record<string, string[]> = {
    'var(--font-display)': ['700'],
    'var(--font-mono)': ['400', '700'],
    'var(--font-ui)': ['400', '600'],
  };

  it('never asks a bundled family for a weight that is not shipped', () => {
    for (const r of rules) {
      const family = r.declarations['font-family'];
      const weight = r.declarations['font-weight'];
      if (family === undefined || weight === undefined || weight === 'inherit') continue;
      expect(ALLOWED[family], `${r.file}: ${r.selector}`).toContain(weight);
    }
  });

  it('measures the terminal cell only in faces the font imports ship', () => {
    const imports = read(`${srcRoot}/shell/fonts.ts`);
    for (const spec of FONT_LOAD_SPECS) {
      const [weight = '', , family = ''] = spec.split(' ');
      const pkg = family.replace(/'/g, '').toLowerCase();
      const dir = spec.includes('JetBrains') ? 'jetbrains-mono' : pkg;
      expect(imports, spec).toContain(`@fontsource/${dir}/${weight}.css`);
    }
  });
});
