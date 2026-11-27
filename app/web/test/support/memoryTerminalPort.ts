import type { TerminalPort, TerminalStream, TerminalStreamListener } from '../../src/ipc/terminalPort';
import {
  parseCursor,
  parseHostId,
  type Cursor,
  type DecimalU64,
  type HostId,
  type TermId,
} from '../../src/model/ids';
import {
  parseDataFrame,
  parseTerminalStreamEvent,
  type AttachRequestOptions,
  type CloseReason,
  type Display,
  type Fidelity,
  type TerminalSize,
  type TerminalStreamEvent,
} from '../../src/model/terminal';

export type SentCommand =
  | { op: 'attach'; terminal: TermId; options: AttachRequestOptions }
  | { op: 'resume'; cursor: Cursor }
  | { op: 'detach'; attachment: DecimalU64 }
  | { op: 'ack'; cursor: Cursor }
  | { op: 'input'; attachment: DecimalU64; bytes: Uint8Array }
  | {
      op: 'automaticReply';
      attachment: DecimalU64;
      epoch: DecimalU64;
      ownershipGeneration: DecimalU64;
      bytes: Uint8Array;
    }
  | { op: 'resize'; attachment: DecimalU64; size: TerminalSize; ownershipGeneration: DecimalU64 }
  | { op: 'focus'; attachment: DecimalU64 }
  | { op: 'setDisplay'; attachment: DecimalU64; display: Display }
  | { op: 'requestOwnership'; attachment: DecimalU64 }
  | { op: 'close' };

export type CommandOp = SentCommand['op'];

/** A command held by `holdNext`, settled by the test. */
export interface HeldCommand {
  resolve(): void;
  reject(message: string): void;
}

type Plan = { kind: 'reject'; message: string } | { kind: 'hold'; promise: Promise<void> };

export class MemoryTerminalStream implements TerminalStream {
  readonly sent: SentCommand[] = [];
  private open = true;
  private readonly plans = new Map<CommandOp, Plan[]>();

  constructor(
    readonly hostId: HostId,
    private readonly listener: TerminalStreamListener,
    private readonly port: MemoryTerminalPort,
  ) {}

  emit(event: TerminalStreamEvent): void {
    if (this.open) this.listener(event);
  }

  /** The next command of this op rejects with `message`. */
  rejectNext(op: CommandOp, message: string): void {
    this.plan(op, { kind: 'reject', message });
  }

  /** The next command of this op stays pending until the returned handle settles it. */
  holdNext(op: CommandOp): HeldCommand {
    let settle!: { resolve(): void; reject(error: Error): void };
    const promise = new Promise<void>((resolve, reject) => {
      settle = { resolve, reject };
    });
    this.plan(op, { kind: 'hold', promise });
    return {
      resolve: () => settle.resolve(),
      reject: (message) => settle.reject(new Error(message)),
    };
  }

  listenerCount(): number {
    return this.open ? 1 : 0;
  }

  commands(op: CommandOp): SentCommand[] {
    return this.sent.filter((command) => command.op === op);
  }

  attach(terminal: TermId, options: AttachRequestOptions): Promise<void> {
    return this.record({ op: 'attach', terminal, options });
  }
  resume(cursor: Cursor): Promise<void> {
    return this.record({ op: 'resume', cursor });
  }
  detach(attachment: DecimalU64): Promise<void> {
    return this.record({ op: 'detach', attachment });
  }
  ack(cursor: Cursor): Promise<void> {
    return this.record({ op: 'ack', cursor });
  }
  input(attachment: DecimalU64, bytes: Uint8Array): Promise<void> {
    return this.record({ op: 'input', attachment, bytes });
  }
  automaticReply(
    attachment: DecimalU64,
    epoch: DecimalU64,
    ownershipGeneration: DecimalU64,
    bytes: Uint8Array,
  ): Promise<void> {
    return this.record({ op: 'automaticReply', attachment, epoch, ownershipGeneration, bytes });
  }
  resize(
    attachment: DecimalU64,
    size: TerminalSize,
    ownershipGeneration: DecimalU64,
  ): Promise<void> {
    return this.record({ op: 'resize', attachment, size, ownershipGeneration });
  }
  focus(attachment: DecimalU64): Promise<void> {
    return this.record({ op: 'focus', attachment });
  }
  setDisplay(attachment: DecimalU64, display: Display): Promise<void> {
    return this.record({ op: 'setDisplay', attachment, display });
  }
  requestOwnership(attachment: DecimalU64): Promise<void> {
    return this.record({ op: 'requestOwnership', attachment });
  }
  close(): Promise<void> {
    const result = this.record({ op: 'close' });
    this.open = false;
    return result;
  }

  private plan(op: CommandOp, plan: Plan): void {
    const plans = this.plans.get(op) ?? [];
    plans.push(plan);
    this.plans.set(op, plans);
  }

  private record(command: SentCommand): Promise<void> {
    this.sent.push(command);
    const plan = this.plans.get(command.op)?.shift();
    let result: Promise<void>;
    if (plan?.kind === 'reject') {
      result = Promise.reject(new Error(plan.message));
    } else if (plan?.kind === 'hold') {
      result = plan.promise;
    } else if (!this.port.isConnected(this.hostId)) {
      result = Promise.reject(new Error('not connected'));
    } else {
      result = Promise.resolve();
    }
    this.port.onCommand?.(this, command);
    return result;
  }
}

export class MemoryTerminalPort implements TerminalPort {
  readonly streams: MemoryTerminalStream[] = [];
  /** Called synchronously after each command is recorded. */
  onCommand: ((stream: MemoryTerminalStream, command: SentCommand) => void) | null = null;
  /** Called after the connection event and before `openStream` resolves. */
  onOpen: ((stream: MemoryTerminalStream) => void) | null = null;
  /** The listeners of opens that `failNextOpen` refused, for tests that feed them late events. */
  readonly refusedListeners: TerminalStreamListener[] = [];
  private readonly connected = new Map<HostId, boolean>();
  private readonly refusals: string[] = [];

  /** The next `openStream` rejects with `message`, after delivering its connection event. */
  failNextOpen(message: string): void {
    this.refusals.push(message);
  }

  isConnected(hostId: HostId): boolean {
    return this.connected.get(hostId) ?? true;
  }

  setConnected(hostId: HostId, connected: boolean): void {
    this.connected.set(hostId, connected);
    for (const stream of this.streams) {
      if (stream.hostId === hostId) stream.emit(connectionEvent(connected));
    }
  }

  openStream(hostId: HostId, listener: TerminalStreamListener): Promise<TerminalStream> {
    const refusal = this.refusals.shift();
    if (refusal !== undefined) {
      this.refusedListeners.push(listener);
      listener(connectionEvent(this.isConnected(hostId)));
      return Promise.reject(new Error(refusal));
    }
    const stream = new MemoryTerminalStream(hostId, listener, this);
    this.streams.push(stream);
    stream.emit(connectionEvent(this.isConnected(hostId)));
    this.onOpen?.(stream);
    return Promise.resolve(stream);
  }
}

export const HOST_ID: HostId = parseHostId('11111111-1111-4111-8111-111111111111');

type U64Input = number | string;

export function cursor(attachment: U64Input, epoch: U64Input, seq: U64Input): Cursor {
  return parseCursor({
    attachment: String(attachment),
    epoch: String(epoch),
    seq: String(seq),
  });
}

function wire(cursorValue: Cursor): Cursor {
  return { ...cursorValue };
}

export interface StartedOptions {
  cols?: number;
  rows?: number;
  fidelity?: Fidelity;
  orderedResizes?: boolean;
  backendGeneration?: U64Input;
}

export function startedEvent(at: Cursor, options: StartedOptions = {}): TerminalStreamEvent {
  return parseTerminalStreamEvent({
    type: 'started',
    cursor: wire(at),
    backend_generation: String(options.backendGeneration ?? 1),
    cols: options.cols ?? 80,
    rows: options.rows ?? 24,
    fidelity: options.fidelity ?? 'exact',
    ordered_resizes: options.orderedResizes ?? true,
  });
}

/** Builds a data frame with its 24-byte big-endian header and parses it. */
export function dataEvent(at: Cursor, text: string): TerminalStreamEvent {
  const payload = new TextEncoder().encode(text);
  const frame = new Uint8Array(24 + payload.length);
  const view = new DataView(frame.buffer);
  view.setBigUint64(0, BigInt(at.attachment), false);
  view.setBigUint64(8, BigInt(at.epoch), false);
  view.setBigUint64(16, BigInt(at.seq), false);
  frame.set(payload, 24);
  return parseDataFrame(frame);
}

export function resizedEvent(at: Cursor, cols: number, rows: number): TerminalStreamEvent {
  return parseTerminalStreamEvent({ type: 'resized', cursor: wire(at), cols, rows });
}

export function factsEvent(
  at: Cursor,
  display: Display = {
    foreground: '#d4d4d4',
    background: '#1e1e1e',
    cursor_color: '#d4d4d4',
    cell_width: 8,
    cell_height: 17,
  },
): TerminalStreamEvent {
  return parseTerminalStreamEvent({ type: 'facts', cursor: wire(at), display });
}

export function resyncedEvent(at: Cursor): TerminalStreamEvent {
  return parseTerminalStreamEvent({ type: 'resynced', cursor: wire(at) });
}

export function closedEvent(at: Cursor, reason: CloseReason): TerminalStreamEvent {
  return parseTerminalStreamEvent({ type: 'closed', cursor: wire(at), reason });
}

export function lostEvent(at: Cursor): TerminalStreamEvent {
  return parseTerminalStreamEvent({ type: 'lost', cursor: wire(at) });
}

export function attachedEvent(request: U64Input, attachment: U64Input): TerminalStreamEvent {
  return parseTerminalStreamEvent({
    type: 'attached',
    request: String(request),
    attachment: String(attachment),
  });
}

export function resumedEvent(
  request: U64Input,
  attachment: U64Input,
  replayThrough: Cursor | null,
): TerminalStreamEvent {
  return parseTerminalStreamEvent({
    type: 'resumed',
    request: String(request),
    attachment: String(attachment),
    replay_through: replayThrough === null ? null : wire(replayThrough),
  });
}

export function gapEvent(attachment: U64Input, request: U64Input | null): TerminalStreamEvent {
  return parseTerminalStreamEvent({
    type: 'gap',
    attachment: String(attachment),
    request: request === null ? null : String(request),
  });
}

export function ownershipEvent(
  attachment: U64Input,
  generation: U64Input,
  owner: U64Input | null,
): TerminalStreamEvent {
  return parseTerminalStreamEvent({
    type: 'ownership',
    attachment: String(attachment),
    ownership_generation: String(generation),
    owner: owner === null ? null : String(owner),
  });
}

export function errorEvent(options: {
  code: string;
  message?: string;
  request?: U64Input | null;
  attachment?: U64Input | null;
}): TerminalStreamEvent {
  const { request = null, attachment = null } = options;
  return parseTerminalStreamEvent({
    type: 'error',
    code: options.code,
    message: options.message ?? options.code,
    request: request === null ? null : String(request),
    attachment: attachment === null ? null : String(attachment),
  });
}

export function connectionEvent(connected: boolean): TerminalStreamEvent {
  return parseTerminalStreamEvent({ type: 'connection', connected });
}
