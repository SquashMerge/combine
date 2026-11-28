import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { COLOR } from '../src/model/tokens';

type Kind = 'text' | 'ui';

interface Pair {
  kind: Kind;
  fg: string;
  bg: string;
}

const HELD_OPACITY = 0.85;

const IN_USE: readonly Pair[] = [
  { kind: 'text', fg: '--text', bg: '--ink' },
  { kind: 'text', fg: '--text', bg: '--surface' },
  { kind: 'text', fg: '--text', bg: '--raised' },
  { kind: 'text', fg: '--text-muted', bg: '--ink' },
  { kind: 'text', fg: '--text-muted', bg: '--surface' },
  { kind: 'text', fg: '--text-muted', bg: '--raised' },
  { kind: 'text', fg: '--text-muted at 0.85', bg: '--surface at 0.85' },
  { kind: 'text', fg: '--ink', bg: '--amber' },
  { kind: 'text', fg: '--ink', bg: '--purple' },
  { kind: 'text', fg: '--danger', bg: '--surface' },
  { kind: 'text', fg: '--danger', bg: '--raised' },
  { kind: 'text', fg: '--danger', bg: '--ink' },
  { kind: 'text', fg: '--lavender', bg: '--surface' },
  { kind: 'ui', fg: '--control-border', bg: '--surface' },
  { kind: 'ui', fg: '--control-border', bg: '--raised' },
  { kind: 'ui', fg: '--control-border', bg: '--ink' },
  { kind: 'ui', fg: '--purple', bg: '--surface' },
  { kind: 'ui', fg: '--purple', bg: '--ink' },
  { kind: 'ui', fg: '--purple', bg: '--raised' },
  { kind: 'ui', fg: '--lavender', bg: '--surface' },
  { kind: 'ui', fg: '--lavender', bg: '--raised' },
  { kind: 'ui', fg: '--lavender', bg: '--ink' },
  { kind: 'ui', fg: '--amber', bg: '--surface' },
  { kind: 'ui', fg: '--amber', bg: '--raised' },
  { kind: 'ui', fg: '--success', bg: '--surface' },
];

const NEVER_USED: readonly Pair[] = [
  { kind: 'text', fg: '--text', bg: '--purple' },
  { kind: 'text', fg: '--text-muted', bg: '--purple' },
  { kind: 'text', fg: '--control-border', bg: '--surface' },
  { kind: 'text', fg: '--control-border', bg: '--raised' },
  { kind: 'ui', fg: '--lavender', bg: '--purple' },
  { kind: 'ui', fg: '--lavender', bg: '--amber' },
];

type Rgb = readonly [number, number, number];

function parse(hex: string): Rgb {
  const n = Number.parseInt(hex.slice(1, 7), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/** The colour a token shows at `alpha` opacity over ink. */
function composite(color: Rgb, alpha: number): Rgb {
  const ink = parse(COLOR.ink);
  return color.map((channel, i) => alpha * channel + (1 - alpha) * (ink[i] ?? 0)) as unknown as Rgb;
}

function resolveToken(reference: string): Rgb {
  const [name = '', at] = reference.split(' at ');
  const key = name.slice(2).replace(/-([a-z])/g, (_, letter: string) => letter.toUpperCase());
  const hex = (COLOR as Record<string, string>)[key];
  if (hex === undefined) throw new Error(`no colour token ${name}`);
  const rgb = parse(hex);
  return at === undefined ? rgb : composite(rgb, Number(at));
}

function luminance([r, g, b]: Rgb): number {
  const [lr, lg, lb] = [r, g, b].map((channel) => {
    const value = channel / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  }) as [number, number, number];
  return 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
}

function ratio(pair: Pair): string {
  const a = luminance(resolveToken(pair.fg));
  const b = luminance(resolveToken(pair.bg));
  return ((Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05)).toFixed(2);
}

const minimum = (kind: Kind): string => (kind === 'text' ? '4.5' : '3');
const cell = (reference: string): string => `\`${reference}\``;

const docPath = resolve(dirname(fileURLToPath(import.meta.url)), '../../../docs/design/contrast.md');
const doc = readFileSync(docPath, 'utf8');

function rowsUnder(heading: string): string[][] {
  const start = doc.indexOf(`\n${heading}\n`);
  if (start < 0) throw new Error(`contrast.md has no "${heading}" section`);
  const rest = doc.slice(start + heading.length + 2);
  const end = rest.search(/\n## /);
  const section = end < 0 ? rest : rest.slice(0, end);
  return section
    .split('\n')
    .filter((line) => line.startsWith('|'))
    .map((line) => line.split('|').slice(1, -1).map((part) => part.trim()))
    .filter((cells) => cells[0] !== 'Kind' && !/^-+$/.test(cells[0] ?? ''));
}

const identity = (cells: readonly string[]): string => cells.slice(0, 3).join(' | ');
const expectedIdentity = (pair: Pair): string => [pair.kind, cell(pair.fg), cell(pair.bg)].join(' | ');

describe.each([
  ['## Pairs in use', IN_USE, true],
  ['## Pairs never used', NEVER_USED, false],
] as const)('docs/design/contrast.md %s', (heading, pairs, inUse) => {
  const rows = rowsUnder(heading);

  it('lists exactly the pairs the test holds', () => {
    expect(rows.map(identity).sort()).toEqual(pairs.map(expectedIdentity).sort());
  });

  it.each(pairs.map((pair) => [expectedIdentity(pair), pair] as const))(
    'records the computed ratio of %s',
    (_name, pair) => {
      const row = rows.find((cells) => identity(cells) === expectedIdentity(pair));
      const expected = ratio(pair);
      expect(row?.[3], `expected Ratio cell ${expected}`).toBe(expected);
      if (!inUse) return;
      expect(row?.[4], `expected Minimum cell ${minimum(pair.kind)}`).toBe(minimum(pair.kind));
      expect(Number(expected), 'meets its minimum').toBeGreaterThanOrEqual(Number(minimum(pair.kind)));
    },
  );
});

describe('held layout', () => {
  it('composites at the held tile opacity the stylesheet uses', () => {
    const css = readFileSync(resolve(dirname(docPath), '../../app/web/src/shell/shell.css'), 'utf8');
    expect(css).toMatch(new RegExp(`\\.layout\\[data-held\\] \\.tile \\{[^}]*opacity: ${HELD_OPACITY}`));
  });
});
