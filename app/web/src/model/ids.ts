export class ModelError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ModelError';
  }
}

export type DecimalU64 = string & { readonly __brand: 'DecimalU64' };
export type HostId = string & { readonly __brand: 'HostId' };
export type TermId = string & { readonly __brand: 'TermId' };

export interface Cursor {
  attachment: DecimalU64;
  epoch: DecimalU64;
  seq: DecimalU64;
}

const U64_MAX_BIGINT = 18446744073709551615n;
const DECIMAL_U64 = /^(0|[1-9][0-9]{0,19})$/;
const HOST_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export const U64_MAX = '18446744073709551615' as DecimalU64;

export function isDecimalU64(value: unknown): value is DecimalU64 {
  return (
    typeof value === 'string' && DECIMAL_U64.test(value) && BigInt(value) <= U64_MAX_BIGINT
  );
}

export function parseDecimalU64(value: unknown): DecimalU64 {
  if (typeof value !== 'string') {
    throw new ModelError(`expected a decimal u64 string, got ${typeof value}`);
  }
  if (!isDecimalU64(value)) {
    throw new ModelError(`not a decimal u64: ${JSON.stringify(value)}`);
  }
  return value;
}

export function compareDecimalU64(a: DecimalU64, b: DecimalU64): -1 | 0 | 1 {
  if (a.length !== b.length) return a.length < b.length ? -1 : 1;
  if (a === b) return 0;
  return a < b ? -1 : 1;
}

export function formatDecimalU64(value: DecimalU64): string {
  return value;
}

export function decimalU64FromBigInt(value: bigint): DecimalU64 {
  if (value < 0n || value > U64_MAX_BIGINT) {
    throw new ModelError(`u64 out of range: ${value.toString()}`);
  }
  return value.toString() as DecimalU64;
}

export function decimalU64ToBigInt(value: DecimalU64): bigint {
  return BigInt(value);
}

/** The value as a plain object, or a ModelError naming `what`. The one object guard the parsers share. */
export function asRecord(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new ModelError(`${what} must be an object`);
  }
  return value as Record<string, unknown>;
}

export function parseCursor(value: unknown): Cursor {
  const record = asRecord(value, 'cursor');
  return {
    attachment: parseDecimalU64(record['attachment']),
    epoch: parseDecimalU64(record['epoch']),
    seq: parseDecimalU64(record['seq']),
  };
}

export function parseHostId(value: unknown, what = 'host id'): HostId {
  if (typeof value !== 'string' || !HOST_ID.test(value)) {
    throw new ModelError(`${what} must be a lowercase UUID: ${JSON.stringify(value)}`);
  }
  return value as HostId;
}

export function parseTermId(value: unknown, what = 'terminal id'): TermId {
  if (typeof value !== 'string' || value === '') {
    throw new ModelError(`${what} must be a non-empty string: ${JSON.stringify(value)}`);
  }
  return value as TermId;
}
