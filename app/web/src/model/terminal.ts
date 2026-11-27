import {
  ModelError,
  asRecord,
  compareDecimalU64,
  decimalU64FromBigInt,
  parseCursor,
  parseDecimalU64,
  type Cursor,
  type DecimalU64,
} from './ids';

export interface TerminalSize {
  cols: number;
  rows: number;
}

export interface Display {
  foreground: string;
  background: string;
  cursor_color: string;
  cell_width: number;
  cell_height: number;
}

export type Fidelity = 'exact' | 'inexact';
export type CloseReason = 'detached' | 'exited';

export interface AttachRequestOptions {
  cols: number;
  rows: number;
  display: Display;
  observe: boolean;
  history: number;
}

export type StreamEvent =
  | {
      type: 'started';
      cursor: Cursor;
      backend_generation: DecimalU64;
      cols: number;
      rows: number;
      fidelity: Fidelity;
      ordered_resizes: boolean;
    }
  | { type: 'data'; cursor: Cursor; bytes: Uint8Array }
  | { type: 'resized'; cursor: Cursor; cols: number; rows: number }
  | { type: 'facts'; cursor: Cursor; display: Display }
  | { type: 'resynced'; cursor: Cursor }
  | { type: 'closed'; cursor: Cursor; reason: CloseReason }
  | { type: 'lost'; cursor: Cursor };

export type ControlEvent =
  | { type: 'attached'; request: DecimalU64; attachment: DecimalU64 }
  | {
      type: 'resumed';
      request: DecimalU64;
      attachment: DecimalU64;
      replay_through: Cursor | null;
    }
  | { type: 'gap'; attachment: DecimalU64; request: DecimalU64 | null }
  | {
      type: 'ownership';
      attachment: DecimalU64;
      ownership_generation: DecimalU64;
      owner: DecimalU64 | null;
    }
  | {
      type: 'error';
      code: string;
      message: string;
      request: DecimalU64 | null;
      attachment: DecimalU64 | null;
    };

/**
 * Whether the host's session is ready for attachments, and the backend instance its daemon
 * serves: `null` while not connected and for a daemon that names none. A session is ready only
 * once its daemon has named the instance, so a pinned panel can tell before it attaches.
 */
export interface ConnectionEvent {
  type: 'connection';
  connected: boolean;
  instance: string | null;
}

export type TerminalStreamEvent = StreamEvent | ControlEvent | ConnectionEvent;

export const DATA_HEADER_BYTES = 24;

const MAX_DIMENSION = 65535;
const COLOUR = /^#[0-9a-fA-F]{6}$/;

function parseIntegerInRange(value: unknown, min: number, what: string): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < min || value > MAX_DIMENSION) {
    throw new ModelError(`${what} must be an integer from ${min} to ${MAX_DIMENSION}`);
  }
  return value;
}

function parseColour(value: unknown, what: string): string {
  if (typeof value !== 'string' || !COLOUR.test(value)) {
    throw new ModelError(`${what} must be a #rrggbb colour: ${JSON.stringify(value)}`);
  }
  return value;
}

function parseBoolean(value: unknown, what: string): boolean {
  if (typeof value !== 'boolean') {
    throw new ModelError(`${what} must be a boolean`);
  }
  return value;
}

function parseNullableU64(value: unknown): DecimalU64 | null {
  return value === null ? null : parseDecimalU64(value);
}

function parseFidelity(value: unknown): Fidelity {
  if (value !== 'exact' && value !== 'inexact') {
    throw new ModelError(`unknown fidelity: ${JSON.stringify(value)}`);
  }
  return value;
}

function parseCloseReason(value: unknown): CloseReason {
  if (value !== 'detached' && value !== 'exited') {
    throw new ModelError(`unknown close reason: ${JSON.stringify(value)}`);
  }
  return value;
}

export function parseDisplay(value: unknown): Display {
  const record = asRecord(value, 'display');
  return {
    foreground: parseColour(record['foreground'], 'foreground'),
    background: parseColour(record['background'], 'background'),
    cursor_color: parseColour(record['cursor_color'], 'cursor_color'),
    cell_width: parseIntegerInRange(record['cell_width'], 0, 'cell_width'),
    cell_height: parseIntegerInRange(record['cell_height'], 0, 'cell_height'),
  };
}

export function parseTerminalStreamEvent(
  value: unknown,
): Exclude<TerminalStreamEvent, { type: 'data' }> {
  const record = asRecord(value, 'terminal stream event');
  switch (record['type']) {
    case 'started':
      return {
        type: 'started',
        cursor: parseCursor(record['cursor']),
        backend_generation: parseDecimalU64(record['backend_generation']),
        cols: parseIntegerInRange(record['cols'], 1, 'cols'),
        rows: parseIntegerInRange(record['rows'], 1, 'rows'),
        fidelity: parseFidelity(record['fidelity']),
        ordered_resizes: parseBoolean(record['ordered_resizes'], 'ordered_resizes'),
      };
    case 'resized':
      return {
        type: 'resized',
        cursor: parseCursor(record['cursor']),
        cols: parseIntegerInRange(record['cols'], 1, 'cols'),
        rows: parseIntegerInRange(record['rows'], 1, 'rows'),
      };
    case 'facts':
      return {
        type: 'facts',
        cursor: parseCursor(record['cursor']),
        display: parseDisplay(record['display']),
      };
    case 'resynced':
      return { type: 'resynced', cursor: parseCursor(record['cursor']) };
    case 'closed':
      return {
        type: 'closed',
        cursor: parseCursor(record['cursor']),
        reason: parseCloseReason(record['reason']),
      };
    case 'lost':
      return { type: 'lost', cursor: parseCursor(record['cursor']) };
    case 'attached':
      return {
        type: 'attached',
        request: parseDecimalU64(record['request']),
        attachment: parseDecimalU64(record['attachment']),
      };
    case 'resumed':
      return {
        type: 'resumed',
        request: parseDecimalU64(record['request']),
        attachment: parseDecimalU64(record['attachment']),
        replay_through:
          record['replay_through'] === null ? null : parseCursor(record['replay_through']),
      };
    case 'gap':
      return {
        type: 'gap',
        attachment: parseDecimalU64(record['attachment']),
        request: parseNullableU64(record['request']),
      };
    case 'ownership':
      return {
        type: 'ownership',
        attachment: parseDecimalU64(record['attachment']),
        ownership_generation: parseDecimalU64(record['ownership_generation']),
        owner: parseNullableU64(record['owner']),
      };
    case 'error': {
      const code = record['code'];
      const message = record['message'];
      if (typeof code !== 'string' || code === '') {
        throw new ModelError('error code must be a non-empty string');
      }
      if (typeof message !== 'string') {
        throw new ModelError('error message must be a string');
      }
      return {
        type: 'error',
        code,
        message,
        request: parseNullableU64(record['request']),
        attachment: parseNullableU64(record['attachment']),
      };
    }
    case 'connection': {
      const instance = record['instance'];
      if (instance !== null && (typeof instance !== 'string' || instance === '')) {
        throw new ModelError('connection instance must be a non-empty string or null');
      }
      return {
        type: 'connection',
        connected: parseBoolean(record['connected'], 'connected'),
        instance,
      };
    }
    default:
      throw new ModelError(`unknown terminal stream event type: ${JSON.stringify(record['type'])}`);
  }
}

export function parseDataFrame(frame: Uint8Array): Extract<StreamEvent, { type: 'data' }> {
  if (frame.byteLength < DATA_HEADER_BYTES) {
    throw new ModelError(
      `data frame needs at least ${DATA_HEADER_BYTES} bytes, got ${frame.byteLength}`,
    );
  }
  const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
  return {
    type: 'data',
    cursor: {
      attachment: decimalU64FromBigInt(view.getBigUint64(0, false)),
      epoch: decimalU64FromBigInt(view.getBigUint64(8, false)),
      seq: decimalU64FromBigInt(view.getBigUint64(16, false)),
    },
    bytes: frame.subarray(DATA_HEADER_BYTES),
  };
}

export function cursorAfter(a: Cursor, b: Cursor): boolean {
  if (a.attachment !== b.attachment) return false;
  const epoch = compareDecimalU64(a.epoch, b.epoch);
  if (epoch !== 0) return epoch > 0;
  return compareDecimalU64(a.seq, b.seq) > 0;
}

export function sameCursor(a: Cursor, b: Cursor): boolean {
  return a.attachment === b.attachment && a.epoch === b.epoch && a.seq === b.seq;
}

export function sameSize(a: TerminalSize, b: TerminalSize): boolean {
  return a.cols === b.cols && a.rows === b.rows;
}

export function sameDisplay(a: Display, b: Display): boolean {
  return (
    a.foreground === b.foreground &&
    a.background === b.background &&
    a.cursor_color === b.cursor_color &&
    a.cell_width === b.cell_width &&
    a.cell_height === b.cell_height
  );
}
