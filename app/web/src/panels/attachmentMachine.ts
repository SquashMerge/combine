import type { Cursor, DecimalU64, TermId } from '../model/ids';
import {
  cursorAfter,
  sameCursor,
  sameDisplay,
  type AttachRequestOptions,
  type Display,
  type Fidelity,
  type StreamEvent,
  type TerminalSize,
  type TerminalStreamEvent,
} from '../model/terminal';
import type { CellSize } from '../renderers/terminal/adapter';
import { DEFAULT_CELL, FALLBACK_SIZE, displayFrom, fitGrid } from '../renderers/terminal/geometry';
import { TERMINAL_HISTORY_LINES, TERMINAL_THEME } from '../renderers/terminal/settings';

export const ACK_INTERVAL_MS = 16;
export const RESIZE_DEBOUNCE_MS = 100;
export const DISPLAY_DEBOUNCE_MS = 250;

export type Phase = 'hidden' | 'waiting' | 'attaching' | 'resuming' | 'live' | 'detaching' | 'ended';
export type TimerName = 'ack' | 'resize' | 'display';
export type PanelStatus =
  | { kind: 'exited' }
  | { kind: 'lost' }
  | { kind: 'closed' }
  | { kind: 'error'; code: string };

export type MachineInput =
  | { kind: 'show' }
  | { kind: 'hide' }
  | { kind: 'event'; event: TerminalStreamEvent }
  | { kind: 'applied'; cursor: Cursor }
  | { kind: 'sendFailed'; op: 'attach' | 'resume'; notConnected: boolean }
  | { kind: 'userInput'; bytes: Uint8Array }
  | { kind: 'automaticReply'; bytes: Uint8Array }
  | { kind: 'focusReport'; bytes: Uint8Array }
  | { kind: 'domFocus' }
  | { kind: 'containerSize'; width: number; height: number }
  | { kind: 'cellSize'; cell: CellSize }
  | { kind: 'takeInput' }
  | { kind: 'reattach' }
  | { kind: 'timer'; timer: TimerName };

export type MachineEffect =
  | { kind: 'attach'; terminal: TermId; options: AttachRequestOptions }
  | { kind: 'resume'; cursor: Cursor }
  | { kind: 'detach'; attachment: DecimalU64 }
  | { kind: 'ack'; cursor: Cursor }
  | { kind: 'input'; attachment: DecimalU64; bytes: Uint8Array }
  | {
      kind: 'automaticReply';
      attachment: DecimalU64;
      epoch: DecimalU64;
      ownershipGeneration: DecimalU64;
      bytes: Uint8Array;
    }
  | {
      kind: 'resize';
      attachment: DecimalU64;
      size: TerminalSize;
      ownershipGeneration: DecimalU64;
    }
  | { kind: 'focus'; attachment: DecimalU64 }
  | { kind: 'setDisplay'; attachment: DecimalU64; display: Display }
  | { kind: 'requestOwnership'; attachment: DecimalU64 }
  /** Disposes the current instance (WebGL addon first), creates one, then inputs `applied`. */
  | { kind: 'createInstance'; size: TerminalSize; cursor: Cursor }
  | { kind: 'disposeInstance' }
  /** The executor inputs `applied` from the write's done callback. */
  | { kind: 'write'; cursor: Cursor; bytes: Uint8Array }
  /** The executor inputs `applied` synchronously, once the resize has run. */
  | { kind: 'resizeInstance'; cursor: Cursor; size: TerminalSize }
  /** (Re)starts that timer. */
  | { kind: 'startTimer'; timer: TimerName; ms: number }
  | { kind: 'cancelTimer'; timer: TimerName };

export interface PanelView {
  phase: Phase;
  connected: boolean;
  ownership: 'self' | 'other' | 'none' | 'unknown';
  ptySize: TerminalSize | null;
  cell: CellSize;
  letterboxed: boolean;
  stale: boolean;
  hasInstance: boolean;
  status: PanelStatus | null;
  fidelity: Fidelity | null;
  outputSkipped: boolean;
  unorderedResizes: boolean;
  canTakeInput: boolean;
  canReattach: boolean;
}

/** A FIFO that never copies on `shift`: replays can queue 16,384 events. */
class EventQueue {
  private items: StreamEvent[] = [];
  private head = 0;

  get length(): number {
    return this.items.length - this.head;
  }

  push(event: StreamEvent): void {
    this.items.push(event);
  }

  shift(): StreamEvent | undefined {
    const event = this.items[this.head];
    if (event === undefined) return undefined;
    this.head += 1;
    if (this.head === this.items.length) {
      this.clear();
    } else if (this.head >= 1024 && this.head * 2 >= this.items.length) {
      this.items = this.items.slice(this.head);
      this.head = 0;
    }
    return event;
  }

  clear(): void {
    this.items = [];
    this.head = 0;
  }
}

interface Instance {
  epoch: DecimalU64;
  size: TerminalSize;
}

interface Ownership {
  generation: DecimalU64;
  owner: DecimalU64 | null;
}

export class AttachmentMachine {
  private visible = false;
  private connected = false;
  private phase: Phase = 'hidden';
  private attachment: DecimalU64 | null = null;
  private detachWhenNamed = false;
  private instance: Instance | null = null;
  private stale = false;
  private applied: Cursor | null = null;
  private received: Cursor | null = null;
  private replayThrough: Cursor | null = null;
  private acked: Cursor | null = null;
  private ackScheduled = false;
  private readonly queue = new EventQueue();
  private applying: StreamEvent | null = null;
  private ownership: Ownership | null = null;
  private fidelity: Fidelity | null = null;
  private orderedResizes = true;
  private outputSkipped = false;
  private status: PanelStatus | null = null;
  private container: { width: number; height: number } | null = null;
  private cell: CellSize = DEFAULT_CELL;
  private effects: MachineEffect[] = [];

  constructor(private readonly terminal: TermId) {}

  handle(input: MachineInput): MachineEffect[] {
    this.effects = [];
    this.step(input);
    this.advance();
    this.maybeResume();
    return this.effects;
  }

  /** True while `cursor` is at or before the resumed attachment's `replay_through`. */
  isReplay(cursor: Cursor): boolean {
    return this.replayThrough !== null && !cursorAfter(cursor, this.replayThrough);
  }

  view(): PanelView {
    const isOwner = this.isOwner();
    const ownership =
      this.ownership === null
        ? 'unknown'
        : isOwner
          ? 'self'
          : this.ownership.owner === null
            ? 'none'
            : 'other';
    return {
      phase: this.phase,
      connected: this.connected,
      ownership,
      ptySize: this.instance?.size ?? null,
      cell: this.cell,
      letterboxed: ownership !== 'self',
      stale: this.stale,
      hasInstance: this.instance !== null,
      status: this.status,
      fidelity: this.fidelity,
      outputSkipped: this.outputSkipped,
      unorderedResizes: !this.orderedResizes,
      canTakeInput: this.phase === 'live' && this.connected && !isOwner,
      canReattach: this.phase === 'ended' && this.status?.kind === 'lost',
    };
  }

  private resumable(): boolean {
    return (
      this.instance !== null &&
      !this.stale &&
      this.applied !== null &&
      this.attachment !== null &&
      this.applied.attachment === this.attachment
    );
  }

  private isOwner(): boolean {
    return this.attachment !== null && this.ownership?.owner === this.attachment;
  }

  private display(): Display {
    return displayFrom(TERMINAL_THEME, this.cell);
  }

  private step(input: MachineInput): void {
    switch (input.kind) {
      case 'show':
        return this.show();
      case 'hide':
        return this.hide();
      case 'event':
        return this.onEvent(input.event);
      case 'applied':
        return this.onApplied(input.cursor);
      case 'sendFailed':
        return this.onSendFailed(input.op, input.notConnected);
      case 'containerSize':
        this.container = { width: input.width, height: input.height };
        return;
      case 'cellSize':
        this.cell = input.cell;
        return;
      case 'timer':
        if (input.timer === 'ack') this.onAckTimer();
        return;
      case 'reattach':
        return this.reattach();
      case 'userInput':
      case 'automaticReply':
      case 'focusReport':
      case 'domFocus':
      case 'takeInput':
        return;
    }
  }

  private show(): void {
    if (this.visible) return;
    this.visible = true;
    this.status = null;
    if (this.phase === 'hidden') {
      if (this.connected) this.attach();
      else this.phase = 'waiting';
    }
  }

  private hide(): void {
    if (!this.visible) return;
    this.visible = false;
    this.ownership = null;
    this.cancelTimer('resize');
    this.cancelTimer('display');
    switch (this.phase) {
      case 'waiting':
        this.dropInstance();
        this.attachment = null;
        this.phase = 'hidden';
        return;
      case 'attaching':
        this.dropInstance();
        if (this.attachment !== null) {
          this.startDetach(this.attachment);
        } else {
          this.detachWhenNamed = true;
          this.phase = 'detaching';
        }
        return;
      case 'resuming':
        this.dropInstance();
        this.attachment = null;
        this.detachWhenNamed = true;
        this.phase = 'detaching';
        return;
      case 'live':
        this.dropInstance();
        if (this.attachment !== null) this.startDetach(this.attachment);
        return;
      case 'ended':
        this.dropInstance();
        this.status = null;
        this.phase = 'hidden';
        return;
      case 'hidden':
      case 'detaching':
        return;
    }
  }

  private onEvent(event: TerminalStreamEvent): void {
    switch (event.type) {
      case 'connection':
        if (event.connected) this.onConnected();
        else this.onDisconnected();
        return;
      case 'attached':
        return this.onAttached(event.attachment);
      case 'gap':
        if (event.request !== null) this.onRequestAnswerFailed();
        else this.onGap(event.attachment);
        return;
      case 'error':
        if (event.request !== null) this.onRequestAnswerFailed(event.code);
        return;
      case 'resumed':
        return this.onResumed(event.attachment, event.replay_through);
      case 'ownership':
        return;
      default:
        this.onStreamEvent(event);
    }
  }

  private onConnected(): void {
    if (this.connected) return;
    this.connected = true;
    if (this.phase === 'waiting' && !this.resumable()) this.attach();
  }

  private onDisconnected(): void {
    if (!this.connected) return;
    this.connected = false;
    this.ownership = null;
    this.cancelTimer('ack');
    this.cancelTimer('resize');
    this.cancelTimer('display');
    this.ackScheduled = false;
    switch (this.phase) {
      case 'attaching':
        this.attachment = null;
        this.queue.clear();
        this.applying = null;
        this.phase = 'waiting';
        return;
      case 'resuming':
      case 'live':
        this.phase = 'waiting';
        return;
      case 'detaching':
        this.attachment = null;
        this.detachWhenNamed = false;
        this.phase = this.visible ? 'waiting' : 'hidden';
        return;
      case 'hidden':
      case 'waiting':
      case 'ended':
        return;
    }
  }

  private onAttached(attachment: DecimalU64): void {
    if (this.detachWhenNamed) {
      this.startDetach(attachment);
    } else if (this.phase === 'attaching' && this.attachment === null) {
      this.attachment = attachment;
    }
  }

  private onResumed(attachment: DecimalU64, replayThrough: Cursor | null): void {
    if (this.phase === 'resuming' && attachment === this.attachment) {
      this.phase = 'live';
      this.replayThrough = replayThrough;
    } else if (this.phase === 'detaching' && this.detachWhenNamed) {
      this.startDetach(attachment);
    }
  }

  /** `gap{request}` and `error{request}`, which answer an attach or a resume. */
  private onRequestAnswerFailed(code?: string): void {
    if (this.phase === 'detaching' && this.detachWhenNamed) {
      this.afterDetach();
    } else if (this.phase === 'resuming') {
      this.finish();
    } else if (this.phase === 'attaching' && this.attachment === null && code !== undefined) {
      this.dropInstance();
      this.status = { kind: 'error', code };
      this.phase = 'ended';
    }
  }

  private onGap(attachment: DecimalU64): void {
    if (this.attachment === null || attachment !== this.attachment) return;
    if (this.phase === 'attaching' || this.phase === 'live') this.finish();
    else if (this.phase === 'detaching') this.afterDetach();
  }

  private onSendFailed(op: 'attach' | 'resume', notConnected: boolean): void {
    const waitingForAttach =
      op === 'attach' && this.phase === 'attaching' && this.attachment === null;
    const waitingForResume = op === 'resume' && this.phase === 'resuming';
    const detachingUnnamed = this.phase === 'detaching' && this.detachWhenNamed;
    if (!waitingForAttach && !waitingForResume && !detachingUnnamed) return;
    if (notConnected) {
      this.onDisconnected();
    } else if (waitingForResume) {
      this.finish();
    } else if (waitingForAttach) {
      this.dropInstance();
      this.status = { kind: 'error', code: 'send_failed' };
      this.phase = 'ended';
    } else {
      this.afterDetach();
    }
  }

  private onStreamEvent(event: StreamEvent): void {
    if (this.attachment === null || event.cursor.attachment !== this.attachment) return;
    if (this.phase === 'attaching' || this.phase === 'live') {
      if (this.received !== null && !cursorAfter(event.cursor, this.received)) return;
      if (event.type === 'started' && this.phase === 'attaching') this.phase = 'live';
      this.queue.push(event);
      this.received = event.cursor;
    } else if (this.phase === 'detaching') {
      if (event.type === 'closed' || event.type === 'lost') {
        // The one unapplied ack: it finishes the attachment on the daemon.
        this.effects.push({ kind: 'ack', cursor: event.cursor });
        this.afterDetach();
      }
    }
  }

  private onApplied(cursor: Cursor): void {
    if (this.applying === null || !sameCursor(cursor, this.applying.cursor)) return;
    this.markApplied(cursor);
  }

  private markApplied(cursor: Cursor): void {
    this.applying = null;
    this.applied = cursor;
    if (this.replayThrough !== null && !cursorAfter(this.replayThrough, cursor)) {
      this.replayThrough = null;
    }
    if (this.connected && this.phase === 'live' && !this.ackScheduled) {
      this.effects.push({ kind: 'startTimer', timer: 'ack', ms: ACK_INTERVAL_MS });
      this.ackScheduled = true;
    }
  }

  private onAckTimer(): void {
    this.ackScheduled = false;
    if (
      this.connected &&
      this.attachment !== null &&
      this.applied !== null &&
      this.applied.attachment === this.attachment &&
      (this.acked === null || !sameCursor(this.applied, this.acked))
    ) {
      this.effects.push({ kind: 'ack', cursor: this.applied });
      this.acked = this.applied;
    }
  }

  /** Applies queued events one at a time, in cursor order. */
  private advance(): void {
    while (this.applying === null) {
      const event = this.queue.shift();
      if (event === undefined) return;
      if (event.type === 'closed' || event.type === 'lost') {
        this.end(event);
        return;
      }
      if (event.type !== 'started') {
        const instance = this.instance;
        if (instance === null || this.stale || event.cursor.epoch !== instance.epoch) continue;
      }
      switch (event.type) {
        case 'started': {
          const size = { cols: event.cols, rows: event.rows };
          this.instance = { epoch: event.cursor.epoch, size };
          this.stale = false;
          this.fidelity = event.fidelity;
          this.orderedResizes = event.ordered_resizes;
          this.outputSkipped = false;
          this.status = null;
          this.applying = event;
          this.effects.push({ kind: 'createInstance', size, cursor: event.cursor });
          break;
        }
        case 'data':
          this.applying = event;
          this.effects.push({ kind: 'write', cursor: event.cursor, bytes: event.bytes });
          break;
        case 'resized': {
          const size = { cols: event.cols, rows: event.rows };
          if (this.instance !== null) this.instance.size = size;
          this.applying = event;
          this.effects.push({ kind: 'resizeInstance', cursor: event.cursor, size });
          break;
        }
        case 'resynced':
          this.outputSkipped = true;
          this.markApplied(event.cursor);
          break;
        case 'facts':
          this.markApplied(event.cursor);
          break;
      }
    }
  }

  /** Resumes from the applied cursor once the queue has drained. */
  private maybeResume(): void {
    if (
      this.phase !== 'waiting' ||
      !this.visible ||
      !this.connected ||
      !this.resumable() ||
      this.applying !== null ||
      this.queue.length > 0 ||
      this.applied === null
    ) {
      return;
    }
    this.effects.push({ kind: 'resume', cursor: this.applied });
    this.phase = 'resuming';
    this.received = this.applied;
    this.replayThrough = null;
  }

  /** The attachment ended on the daemon: ack at once, and keep the screen. */
  private end(event: StreamEvent & { type: 'closed' | 'lost' }): void {
    this.cancelTimer('ack');
    this.ackScheduled = false;
    this.effects.push({ kind: 'ack', cursor: event.cursor });
    this.status =
      event.type === 'lost'
        ? { kind: 'lost' }
        : { kind: event.reason === 'exited' ? 'exited' : 'closed' };
    this.resetStream();
    this.attachment = null;
    this.phase = 'ended';
  }

  private reattach(): void {
    if (this.phase !== 'ended' || this.status?.kind !== 'lost') return;
    this.status = null;
    if (this.connected) this.attach();
    else this.phase = 'waiting';
  }

  private attach(): void {
    const display = this.display();
    const fit = this.container === null ? null : fitGrid(this.container, this.cell);
    this.effects.push({
      kind: 'attach',
      terminal: this.terminal,
      options: {
        ...(fit ?? this.instance?.size ?? FALLBACK_SIZE),
        display,
        observe: false,
        history: TERMINAL_HISTORY_LINES,
      },
    });
    this.phase = 'attaching';
    this.attachment = null;
    this.detachWhenNamed = false;
    this.stale = this.instance !== null;
    this.resetStream();
  }

  /** The attachment is over but the panel stays. */
  private finish(): void {
    this.cancelTimer('ack');
    this.ackScheduled = false;
    this.resetStream();
    this.attachment = null;
    this.stale = this.instance !== null;
    if (this.connected) this.attach();
    else this.phase = 'waiting';
  }

  private startDetach(attachment: DecimalU64): void {
    this.effects.push({ kind: 'detach', attachment });
    this.attachment = attachment;
    this.detachWhenNamed = false;
    this.phase = 'detaching';
  }

  /** The detaching attachment is over. */
  private afterDetach(): void {
    this.attachment = null;
    this.detachWhenNamed = false;
    if (this.visible && this.connected) this.attach();
    else this.phase = this.visible ? 'waiting' : 'hidden';
  }

  private dropInstance(): void {
    if (this.instance !== null) this.effects.push({ kind: 'disposeInstance' });
    this.instance = null;
    this.stale = false;
    this.resetStream();
    this.applied = null;
    this.cancelTimer('ack');
    this.ackScheduled = false;
  }

  /** Forgets everything about the current attachment's stream. */
  private resetStream(): void {
    this.applied = null;
    this.received = null;
    this.replayThrough = null;
    this.acked = null;
    this.ownership = null;
    this.queue.clear();
    this.applying = null;
  }

  private cancelTimer(timer: TimerName): void {
    this.effects.push({ kind: 'cancelTimer', timer });
  }
}
