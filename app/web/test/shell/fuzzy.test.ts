import { describe, expect, it } from 'vitest';
import { MAX_RESULTS, fuzzyFilter, fuzzyScore } from '../../src/shell/fuzzy';

/** The original O(|query| * |text|^2) scorer, kept as the reference the linear one must equal. */
function quadraticScore(query: string, text: string): number | null {
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  if (q === '') return 0;
  let previous: (number | null)[] | null = null;
  for (let j = 0; j < q.length; j += 1) {
    const row: (number | null)[] = new Array<number | null>(t.length).fill(null);
    for (let i = 0; i < t.length; i += 1) {
      if (t[i] !== q[j]) continue;
      let bonus = 0;
      if (i === 0) bonus = 16;
      else if (' /\\-_.:·'.includes(t[i - 1] ?? '')) bonus = 8;
      if (previous === null) {
        row[i] = bonus;
        continue;
      }
      let best: number | null = null;
      for (let k = 0; k < i; k += 1) {
        const before = previous[k];
        if (before === undefined || before === null) continue;
        const gap = i - k - 1;
        const score = before + bonus + (gap === 0 ? 4 : -gap);
        if (best === null || score > best) best = score;
      }
      row[i] = best;
    }
    previous = row;
  }
  let best: number | null = null;
  for (const score of previous ?? []) {
    if (score !== null && score !== undefined && (best === null || score > best)) best = score;
  }
  return best;
}

const item = (label: string, detail = '', group = 0) => ({ label, detail, group });

describe('fuzzy', () => {
  it('subsequence case-insensitive', () => {
    expect(fuzzyScore('MMR', 'mimir')).not.toBeNull();
    expect(fuzzyScore('mir', 'MIMIR')).not.toBeNull();
    const hits = fuzzyFilter([item('Alpha', 'mac /src/alpha')], 'ALPH');
    expect(hits.map((hit) => hit.label)).toEqual(['Alpha']);
  });

  it('matches across the label and the detail', () => {
    expect(fuzzyFilter([item('build', 'mini')], 'bmini')).toHaveLength(1);
  });

  it('ranks prefix and word start first', () => {
    const items = [item('xalpha'), item('z alpha'), item('alpha')];
    expect(fuzzyFilter(items, 'alp').map((hit) => hit.label)).toEqual(['alpha', 'z alpha', 'xalpha']);
  });

  it('scores bonuses and gaps as specified', () => {
    expect(fuzzyScore('a', 'abc')).toBe(16);
    expect(fuzzyScore('b', 'a b')).toBe(8);
    expect(fuzzyScore('ab', 'ab')).toBe(16 + 4);
    expect(fuzzyScore('ac', 'abxc')).toBe(16 - 2);
  });

  it('rejects non-subsequence', () => {
    expect(fuzzyScore('ba', 'ab')).toBeNull();
    expect(fuzzyFilter([item('alpha')], 'zz')).toEqual([]);
  });

  it('breaks ties by group, then label', () => {
    const tied = [item('bx', '', 1), item('bx', '', 0), item('ax', '', 1)];
    expect(fuzzyFilter(tied, 'x').map((hit) => `${hit.group}${hit.label}`)).toEqual([
      '0bx',
      '1ax',
      '1bx',
    ]);
  });

  it('empty query keeps order', () => {
    const items = [item('c'), item('a'), item('b')];
    expect(fuzzyFilter(items, '').map((hit) => hit.label)).toEqual(['c', 'a', 'b']);
    expect(fuzzyFilter(items, '  ').map((hit) => hit.label)).toEqual(['c', 'a', 'b']);
  });

  it('shows at most 50 results', () => {
    const items = Array.from({ length: 80 }, (_, i) => item(`t${i}`));
    expect(fuzzyFilter(items, '')).toHaveLength(MAX_RESULTS);
    expect(fuzzyFilter(items, 't')).toHaveLength(MAX_RESULTS);
  });

  it('ties sort by kind then label', () => {
    const tied = [item('bx', '', 3), item('bx', '', 0), item('ax', '', 3), item('cx', '', 0)];
    expect(fuzzyFilter(tied, 'x').map((hit) => `${hit.group}${hit.label}`)).toEqual(['0bx', '0cx', '3ax', '3bx']);
  });

  it('scores equal the quadratic reference', () => {
    const texts = ['mimir', 'mac /src/alpha', 'a b a b', 'aaaa', 'x-y_z.abc:def', 'Close panel in alpha', ''];
    const queries = ['a', 'ab', 'aa', 'aaa', 'mir', 'ca', 'xyz', 'pa', 'zz', 'cpa', 'b a', 'lpa'];
    for (const text of texts) {
      for (const query of queries) {
        expect(fuzzyScore(query, text), `${query} in ${text}`).toBe(quadraticScore(query, text));
      }
    }
    let seed = 7;
    const next = (): number => {
      seed = (seed * 1103515245 + 12345) % 2147483648;
      return seed;
    };
    const alphabet = 'ab -_/';
    const random = (length: number): string =>
      Array.from({ length }, () => alphabet[next() % alphabet.length]).join('');
    for (let n = 0; n < 300; n += 1) {
      const text = random(1 + (next() % 24));
      const query = random(1 + (next() % 5)).trim();
      expect(fuzzyScore(query, text), `${query} in ${text}`).toBe(quadraticScore(query, text));
    }
  });

  it('scores a 64 KiB text with a 32-character query within the test timeout', () => {
    const text = 'ab'.repeat(32 * 1024);
    expect(fuzzyScore('ab'.repeat(16), text)).not.toBeNull();
  });
});
