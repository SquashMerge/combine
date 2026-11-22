import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { decimalU64ToBigInt, parseCursor, parseDecimalU64 } from '../../src/model/ids';

const fixture: unknown = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'u64-roundtrip.json'), 'utf8'),
);

describe('u64 round trip', () => {
  const file = fixture as Record<string, unknown>;

  it('keeps values above 2^53 exact', () => {
    expect(decimalU64ToBigInt(parseDecimalU64(file['above2pow53']))).toBe(9007199254740993n);
    expect(decimalU64ToBigInt(parseDecimalU64(file['max']))).toBe(18446744073709551615n);
  });

  it('re-serializes to the same JSON', () => {
    const parsed = {
      above2pow53: parseDecimalU64(file['above2pow53']),
      max: parseDecimalU64(file['max']),
      zero: parseDecimalU64(file['zero']),
      cursor: parseCursor(file['cursor']),
      request: parseDecimalU64(file['request']),
      backendGeneration: parseDecimalU64(file['backendGeneration']),
      ownershipGeneration: parseDecimalU64(file['ownershipGeneration']),
    };
    expect(JSON.parse(JSON.stringify(parsed))).toEqual(fixture);
  });

  it('shows the loss a JS number would cause', () => {
    expect(BigInt(Number('9007199254740993'))).not.toBe(9007199254740993n);
  });
});
