import { cursorAfter, type TerminalSize, type TerminalStreamEvent } from '../../src/model/terminal';
import type { Cursor, DecimalU64, HostId, TermId } from '../../src/model/ids';
import {
  HOST_ID,
  attachedEvent,
  closedEvent,
  cursor,
  dataEvent,
  errorEvent,
  gapEvent,
  ownershipEvent,
  resizedEvent,
  resumedEvent,
  startedEvent,
  type MemoryTerminalPort,
  type MemoryTerminalStream,
  type SentCommand,
} from './memoryTerminalPort';

const DEFAULT_PTY_SIZE: TerminalSize = { cols: 80, rows: 24 };
const QUERY = '\x1b[?6n';

interface LoggedEvent {
  cursor: Cursor;
  event: TerminalStreamEvent;
}

interface Attachment {
  id: number;
  terminal: TermId;
  stream: MemoryTerminalStream;
  seq: number;
  started: boolean;
  log: LoggedEvent[];
}

interface Terminal {
  screen: string;
  size: TerminalSize;
  epoch: number;
  generation: number;
  owner: number | null;
  typed: Typed[];
  automatic: string[];
}

export interface Typed {
  attachment: DecimalU64;
  text: string;
}

const decoder = new TextDecoder();

function u64(value: number): DecimalU64 {
  return String(value) as DecimalU64;
}

/**
 * Plays the daemon side of the terminal protocol for the two-panel scenarios. It models only
 * what they need and answers each command synchronously, through `MemoryTerminalPort.onCommand`.
 */
export class ScriptedDaemon {
  /** While true, an attach answers `attached` and `ownership` but holds back `started`. */
  holdStart = false;
  private readonly terminals = new Map<TermId, Terminal>();
  private readonly attachments = new Map<number, Attachment>();
  private nextAttachment = 1;
  private nextRequest = 1;
  private connected = true;

  constructor(
    private readonly port: MemoryTerminalPort,
    private readonly hostId: HostId = HOST_ID,
  ) {
    port.onCommand = (stream, command) => this.handle(stream, command);
  }

  /** The attachment that owns `terminal`, or null. */
  ownerOf(terminal: TermId): DecimalU64 | null {
    const owner = this.terminal(terminal).owner;
    return owner === null ? null : u64(owner);
  }

  generation(terminal: TermId): DecimalU64 {
    return u64(this.terminal(terminal).generation);
  }

  /** Every `input` the daemon received for `terminal`, in order. */
  typed(terminal: TermId): Typed[] {
    return this.terminal(terminal).typed;
  }

  /** Every `automatic_reply` the daemon kept for `terminal`: the owner's, at the current epoch and generation. */
  automatic(terminal: TermId): string[] {
    return this.terminal(terminal).automatic;
  }

  ptySize(terminal: TermId): TerminalSize {
    return this.terminal(terminal).size;
  }

  setScreen(terminal: TermId, text: string): void {
    this.terminal(terminal).screen = text;
  }

  /** Appends output to the screen and the log of every attachment of `terminal`. */
  output(terminal: TermId, text: string): void {
    this.terminal(terminal).screen += text;
    for (const attachment of this.attachmentsOf(terminal)) {
      this.log(attachment, (at) => dataEvent(at, text));
    }
  }

  /** Output that makes xterm answer with a DECXCPR reply. */
  query(terminal: TermId): void {
    this.output(terminal, QUERY);
  }

  /** The connection drops. The log keeps appending, and every owner hands off to null. */
  dropConnection(): void {
    this.connected = false;
    for (const terminal of this.terminals.values()) {
      if (terminal.owner !== null) {
        terminal.owner = null;
        terminal.generation += 1;
      }
    }
    this.port.setConnected(this.hostId, false);
  }

  restoreConnection(): void {
    this.connected = true;
    this.port.setConnected(this.hostId, true);
  }

  /** A daemon restart: every attachment is forgotten, IDs keep counting and epochs start over. */
  restart(hostId: HostId = this.hostId): void {
    if (hostId !== this.hostId) throw new Error('the scripted daemon serves one host');
    this.dropConnection();
    this.attachments.clear();
    for (const terminal of this.terminals.values()) {
      terminal.epoch = 1;
      terminal.owner = null;
    }
    this.restoreConnection();
  }

  /** The backend of `terminal` restarted: every attachment gets a `started` at the next epoch. */
  restartBackend(terminal: TermId): void {
    const state = this.terminal(terminal);
    state.epoch += 1;
    for (const attachment of this.attachmentsOf(terminal)) {
      this.sendStart(attachment);
    }
  }

  /** Sends the `started` an attach held back. */
  releaseStart(): void {
    this.holdStart = false;
    for (const attachment of this.attachments.values()) {
      if (!attachment.started) this.sendStart(attachment);
    }
  }

  private terminal(id: TermId): Terminal {
    let state = this.terminals.get(id);
    if (state === undefined) {
      state = {
        screen: '',
        size: DEFAULT_PTY_SIZE,
        epoch: 1,
        generation: 0,
        owner: null,
        typed: [],
        automatic: [],
      };
      this.terminals.set(id, state);
    }
    return state;
  }

  private attachmentsOf(terminal: TermId): Attachment[] {
    return [...this.attachments.values()].filter((attachment) => attachment.terminal === terminal);
  }

  private handle(stream: MemoryTerminalStream, command: SentCommand): void {
    switch (command.op) {
      case 'attach':
        return this.attach(stream, command.terminal);
      case 'resume':
        return this.resume(stream, command.cursor);
      case 'detach':
        return this.detach(command.attachment);
      case 'input':
        return this.input(command.attachment, command.bytes);
      case 'requestOwnership':
        return this.takeOwnership(command.attachment);
      case 'resize':
        return this.resize(command.attachment, command.size, command.ownershipGeneration);
      case 'automaticReply':
        return this.automaticReply(
          command.attachment,
          command.epoch,
          command.ownershipGeneration,
          command.bytes,
        );
      case 'ack':
      case 'focus':
      case 'setDisplay':
      case 'close':
        return;
    }
  }

  private deliver(attachment: Attachment, event: TerminalStreamEvent): void {
    if (this.connected) attachment.stream.emit(event);
  }

  /** Appends an event to the attachment's log at its next sequence number, and delivers it. */
  private log(attachment: Attachment, build: (at: Cursor) => TerminalStreamEvent): void {
    attachment.seq += 1;
    const at = cursor(attachment.id, this.terminal(attachment.terminal).epoch, attachment.seq);
    const event = build(at);
    attachment.log.push({ cursor: at, event });
    this.deliver(attachment, event);
  }

  private notifyOwnership(terminal: TermId): void {
    const state = this.terminal(terminal);
    for (const attachment of this.attachmentsOf(terminal)) {
      this.deliver(attachment, ownershipEvent(attachment.id, state.generation, state.owner));
    }
  }

  private attach(stream: MemoryTerminalStream, terminal: TermId): void {
    const attachment: Attachment = {
      id: this.nextAttachment++,
      terminal,
      stream,
      seq: -1,
      started: false,
      log: [],
    };
    this.attachments.set(attachment.id, attachment);
    const state = this.terminal(terminal);
    this.deliver(attachment, attachedEvent(this.nextRequest++, attachment.id));
    this.deliver(attachment, ownershipEvent(attachment.id, state.generation, state.owner));
    if (!this.holdStart) this.sendStart(attachment);
  }

  /** `started` at seq 0 of the current epoch, then the first paint. */
  private sendStart(attachment: Attachment): void {
    const state = this.terminal(attachment.terminal);
    attachment.started = true;
    attachment.seq = -1;
    this.log(attachment, (at) =>
      startedEvent(at, { cols: state.size.cols, rows: state.size.rows }),
    );
    this.log(attachment, (at) => dataEvent(at, state.screen));
  }

  private resume(stream: MemoryTerminalStream, from: Cursor): void {
    const request = this.nextRequest++;
    const attachment = this.attachments.get(Number(from.attachment));
    if (attachment === undefined) {
      stream.emit(gapEvent(from.attachment, request));
      return;
    }
    const replay = attachment.log.filter((entry) => cursorAfter(entry.cursor, from));
    const last = replay[replay.length - 1];
    this.deliver(attachment, resumedEvent(request, attachment.id, last?.cursor ?? null));
    for (const entry of replay) this.deliver(attachment, entry.event);
    const state = this.terminal(attachment.terminal);
    this.deliver(attachment, ownershipEvent(attachment.id, state.generation, state.owner));
  }

  private detach(id: DecimalU64): void {
    const attachment = this.attachments.get(Number(id));
    if (attachment === undefined) return;
    this.attachments.delete(attachment.id);
    if (!attachment.started) {
      this.deliver(attachment, gapEvent(attachment.id, null));
    } else {
      this.log(attachment, (at) => closedEvent(at, 'detached'));
    }
    const state = this.terminal(attachment.terminal);
    if (state.owner === attachment.id) {
      state.owner = null;
      state.generation += 1;
      this.notifyOwnership(attachment.terminal);
    }
  }

  private takeOwnership(id: DecimalU64): void {
    const attachment = this.attachments.get(Number(id));
    if (attachment === undefined) return;
    const state = this.terminal(attachment.terminal);
    if (state.owner === attachment.id) return;
    state.owner = attachment.id;
    state.generation += 1;
    this.notifyOwnership(attachment.terminal);
  }

  private input(id: DecimalU64, bytes: Uint8Array): void {
    const attachment = this.attachments.get(Number(id));
    if (attachment === undefined) return;
    this.terminal(attachment.terminal).typed.push({ attachment: id, text: decoder.decode(bytes) });
    this.takeOwnership(id);
  }

  private resize(id: DecimalU64, size: TerminalSize, generation: DecimalU64): void {
    const attachment = this.attachments.get(Number(id));
    if (attachment === undefined) return;
    const state = this.terminal(attachment.terminal);
    if (generation !== u64(state.generation)) {
      this.deliver(
        attachment,
        errorEvent({ code: 'stale_generation', attachment: attachment.id }),
      );
      return;
    }
    state.size = size;
    for (const each of this.attachmentsOf(attachment.terminal)) {
      this.log(each, (at) => resizedEvent(at, size.cols, size.rows));
    }
  }

  private automaticReply(
    id: DecimalU64,
    epoch: DecimalU64,
    generation: DecimalU64,
    bytes: Uint8Array,
  ): void {
    const attachment = this.attachments.get(Number(id));
    if (attachment === undefined) return;
    const state = this.terminal(attachment.terminal);
    if (
      state.owner === attachment.id &&
      epoch === u64(state.epoch) &&
      generation === u64(state.generation)
    ) {
      state.automatic.push(decoder.decode(bytes));
    }
  }
}
