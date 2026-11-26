export interface Searchable {
  label: string;
  detail: string;
  group: number;
}

export const MAX_RESULTS = 50;

const START_BONUS = 16;
const WORD_START_BONUS = 8;
const CONSECUTIVE_BONUS = 4;
const SEPARATORS = ' /\\-_.:·';

function isSeparator(char: string): boolean {
  return SEPARATORS.includes(char);
}

/**
 * Best score of `query` as a case-insensitive subsequence of `text`, or null
 * when it is not one. Every matched character earns a bonus at index 0, after a
 * separator, or right after the previous match; each skipped character between
 * two matches costs 1.
 */
export function fuzzyScore(query: string, text: string): number | null {
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  if (q === '') return 0;
  let previous: (number | null)[] | null = null;
  for (let j = 0; j < q.length; j += 1) {
    const row: (number | null)[] = new Array<number | null>(t.length).fill(null);
    // Best previous[k] + k over k <= i - 2: a skip of i - k - 1 costs that many points.
    let shifted: number | null = null;
    for (let i = 0; i < t.length; i += 1) {
      if (previous !== null && i >= 2) {
        const entering = previous[i - 2];
        if (entering !== undefined && entering !== null) {
          const value = entering + i - 2;
          if (shifted === null || value > shifted) shifted = value;
        }
      }
      if (t[i] !== q[j]) continue;
      let bonus = 0;
      if (i === 0) bonus = START_BONUS;
      else if (isSeparator(t[i - 1] ?? '')) bonus = WORD_START_BONUS;
      if (previous === null) {
        row[i] = bonus;
        continue;
      }
      let best: number | null = null;
      const adjacent = previous[i - 1];
      if (adjacent !== undefined && adjacent !== null) {
        best = adjacent + bonus + CONSECUTIVE_BONUS;
      }
      if (shifted !== null) {
        const score = shifted - i + 1 + bonus;
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

export function fuzzyFilter<T extends Searchable>(
  items: readonly T[],
  query: string,
  limit: number = MAX_RESULTS,
): T[] {
  if (query.trim() === '') return items.slice(0, limit);
  const scored: { item: T; score: number }[] = [];
  for (const item of items) {
    const score = fuzzyScore(query.trim(), `${item.label} ${item.detail}`);
    if (score !== null) scored.push({ item, score });
  }
  scored.sort((a, b) => {
    if (a.score !== b.score) return b.score - a.score;
    if (a.item.group !== b.item.group) return a.item.group - b.item.group;
    if (a.item.label < b.item.label) return -1;
    return a.item.label > b.item.label ? 1 : 0;
  });
  return scored.slice(0, limit).map(({ item }) => item);
}
