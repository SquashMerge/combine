import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { ModelError, parseDecimalU64, type Cursor } from '../../src/model/ids';
import {
  DATA_HEADER_BYTES,
  cursorAfter,
  parseDataFrame,
  parseTerminalStreamEvent,
} from '../../src/model/terminal';

interface NamedIpc {
  name: string;
  ipc: unknown;
}
interface DataEntry {
  name: string;
  frame_hex: string;
  cursor: Cursor;
  payload_hex: string;
}
interface Fixture {
  control: NamedIpc[];
  data: DataEntry[];
  rejected_control: NamedIpc[];
  rejected_data: { name: string; frame_hex: string }[];
}

const fixture = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'terminal-events.json'), 'utf8'),
) as Fixture;

function fromHex(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i += 1) {
    bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

function cursor(attachment: string, epoch: string, seq: string): Cursor {
  return {
    attachment: parseDecimalU64(attachment),
    epoch: parseDecimalU64(epoch),
    seq: parseDecimalU64(seq),
  };
}

describe('terminal stream events', () => {
  it('parses every control event in the fixture', () => {
    expect(fixture.control).toHaveLength(21);
    for (const entry of fixture.control) {
      expect(parseTerminalStreamEvent(entry.ipc), entry.name).toEqual(entry.ipc);
    }
  });

  it('rejects every malformed control event in the fixture', () => {
    expect(fixture.rejected_control.length).toBeGreaterThanOrEqual(5);
    for (const entry of fixture.rejected_control) {
      expect(() => parseTerminalStreamEvent(entry.ipc), entry.name).toThrow(ModelError);
    }
  });

  it('parses data frames in the fixture as big-endian decimal u64s', () => {
    for (const entry of fixture.data) {
      const event = parseDataFrame(fromHex(entry.frame_hex));
      expect(event.type, entry.name).toBe('data');
      expect(event.cursor, entry.name).toEqual(entry.cursor);
      expect(toHex(event.bytes), entry.name).toBe(entry.payload_hex);
    }
  });

  it('parses a frame that is a view into a larger buffer', () => {
    const frame = fromHex(fixture.data[0]!.frame_hex);
    const padded = new Uint8Array(frame.length + 5);
    padded.set(frame, 3);
    const event = parseDataFrame(padded.subarray(3, 3 + frame.length));
    expect(event.cursor).toEqual(fixture.data[0]!.cursor);
    expect(event.bytes.buffer).toBe(padded.buffer);
  });

  it('rejects a data frame shorter than 24 bytes', () => {
    expect(DATA_HEADER_BYTES).toBe(24);
    for (const entry of fixture.rejected_data) {
      expect(() => parseDataFrame(fromHex(entry.frame_hex)), entry.name).toThrow(ModelError);
    }
    expect(() => parseDataFrame(new Uint8Array(0))).toThrow(ModelError);
  });

  it('keeps max u64 cursor values exact', () => {
    const max = '18446744073709551615';
    const entry = fixture.data.find((candidate) => candidate.name === 'max_u64')!;
    expect(parseDataFrame(fromHex(entry.frame_hex)).cursor).toEqual({
      attachment: max,
      epoch: max,
      seq: max,
    });
    const attached = fixture.control.find((candidate) => candidate.name === 'attached_max_u64')!;
    expect(parseTerminalStreamEvent(attached.ipc)).toEqual({
      type: 'attached',
      request: max,
      attachment: max,
    });
  });

  it('cursorAfter orders by epoch, then seq, within one attachment', () => {
    expect(cursorAfter(cursor('7', '1', '2'), cursor('7', '1', '1'))).toBe(true);
    expect(cursorAfter(cursor('7', '1', '1'), cursor('7', '1', '1'))).toBe(false);
    expect(cursorAfter(cursor('7', '1', '1'), cursor('7', '1', '2'))).toBe(false);
    expect(cursorAfter(cursor('7', '2', '0'), cursor('7', '1', '99'))).toBe(true);
    expect(cursorAfter(cursor('7', '1', '99'), cursor('7', '2', '0'))).toBe(false);
    expect(cursorAfter(cursor('8', '9', '9'), cursor('7', '1', '1'))).toBe(false);
    expect(cursorAfter(cursor('7', '9', '9'), cursor('8', '1', '1'))).toBe(false);
  });
});
