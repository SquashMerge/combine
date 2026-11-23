import { describe, expect, it } from 'vitest';
import { MAX_RESULTS, fuzzyFilter, fuzzyScore } from '../../src/shell/fuzzy';

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
});
