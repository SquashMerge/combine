import { describe, expect, it } from 'vitest';
import {
  ModelError,
  U64_MAX,
  compareDecimalU64,
  decimalU64FromBigInt,
  decimalU64ToBigInt,
  formatDecimalU64,
  isDecimalU64,
  parseCursor,
  parseDecimalU64,
  parseHostId,
  parseTermId,
} from '../../src/model/ids';

const HOST = '0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c';

describe('parseDecimalU64', () => {
  it.each(['0', '1', '9007199254740993', '18446744073709551615'])('accepts %s', (text) => {
    expect(parseDecimalU64(text)).toBe(text);
    expect(isDecimalU64(text)).toBe(true);
  });

  it.each([
    '',
    '01',
    '-1',
    '+1',
    ' 1',
    '1 ',
    '1\n',
    '1e3',
    '0x10',
    '1.0',
    '18446744073709551616',
    '99999999999999999999',
    '１',
    1,
    1n,
    null,
    undefined,
  ])('rejects %p', (value) => {
    expect(() => parseDecimalU64(value)).toThrow(ModelError);
    expect(isDecimalU64(value)).toBe(false);
  });
});

describe('compare and format', () => {
  it('orders by length then text', () => {
    const a = parseDecimalU64('9');
    const b = parseDecimalU64('10');
    expect(compareDecimalU64(a, b)).toBe(-1);
    expect(compareDecimalU64(b, a)).toBe(1);
    expect(compareDecimalU64(b, b)).toBe(0);
    expect(compareDecimalU64(parseDecimalU64('9007199254740993'), parseDecimalU64('9007199254740992'))).toBe(1);
    expect(compareDecimalU64(parseDecimalU64('0'), U64_MAX)).toBe(-1);
  });

  it('formats as the identity', () => {
    expect(formatDecimalU64(U64_MAX)).toBe('18446744073709551615');
  });
});

describe('bigint conversion', () => {
  it('round-trips within range', () => {
    expect(decimalU64FromBigInt(18446744073709551615n)).toBe(U64_MAX);
    expect(decimalU64ToBigInt(U64_MAX)).toBe(18446744073709551615n);
  });

  it('rejects out of range', () => {
    expect(() => decimalU64FromBigInt(-1n)).toThrow(ModelError);
    expect(() => decimalU64FromBigInt(18446744073709551616n)).toThrow(ModelError);
  });
});

describe('cursor and id parsers', () => {
  it('parses a cursor and rejects a numeric field', () => {
    const cursor = { attachment: '1', epoch: '2', seq: '3' };
    expect(parseCursor(cursor)).toEqual(cursor);
    expect(() => parseCursor({ ...cursor, seq: 3 })).toThrow(ModelError);
    expect(() => parseCursor(null)).toThrow(ModelError);
  });

  it('accepts only lowercase UUID host ids', () => {
    expect(parseHostId(HOST)).toBe(HOST);
    expect(() => parseHostId(HOST.toUpperCase())).toThrow(ModelError);
    expect(() => parseHostId('')).toThrow(ModelError);
    expect(() => parseHostId(null)).toThrow(ModelError);
  });

  it('accepts only non-empty terminal ids', () => {
    expect(parseTermId('hc:t8')).toBe('hc:t8');
    expect(() => parseTermId('')).toThrow(ModelError);
    expect(() => parseTermId(8)).toThrow(ModelError);
  });
});
