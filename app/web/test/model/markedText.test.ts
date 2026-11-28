import { describe, expect, it } from 'vitest';
import { type MarkedText, plainText } from '../../src/model/markedText';

describe('plainText', () => {
  it('returns a string as is', () => {
    expect(plainText('Loading…')).toBe('Loading…');
    expect(plainText('')).toBe('');
  });

  it('joins string parts and code parts in order', () => {
    const text: MarkedText = ['Panel ', { code: 'a' }, ' is missing'];
    expect(plainText(text)).toBe('Panel a is missing');
  });

  it('joins adjacent code parts and an empty array', () => {
    expect(plainText([{ code: 'x' }, { code: 'y' }])).toBe('xy');
    expect(plainText([])).toBe('');
  });
});
