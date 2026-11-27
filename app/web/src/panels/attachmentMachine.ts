import type { Cursor, DecimalU64, TermId } from '../model/ids';
import {
  cursorAfter,
  sameCursor,
  sameDisplay,
  sameSize,
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
  /** A pinned panel's terminal belongs to a backend instance the host no longer serves. */
  | { kind: 'gone'; reason: 'backendGone' }
  | { kind: 'exited' }
  | { kind: 'lost' }
  | { kind: 'closed' }
  | { kind: 'error'; code: string }
  | { kind: 'streamFailed'; message: string };

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
  | { kind: 'focused'; focused: boolean }
  | { kind: 'streamFailed'; message: string }
  | { kind: 'retryStream' }
  | { kind: 'reattach' }
  | { kind: 'timer'; timer: TimerName };

export type MachineEffect =
  | { kind: 'attach'; terminal: TermId; options: AttachRequestOptions; instance: string | null }
  | { kind: 'resume'; cursor: Cursor; instance: string | null }
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
  /**
   * `ifVacant` is the generation under which an automatic claim saw no owner: the daemon grants
   * it only if that is still so. `null` is a manual "Take input", which always takes ownership.
   */
  | { kind: 'requestOwnership'; attachment: DecimalU64; ifVacant: DecimalU64 | null }
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
  canRetry: boolean;
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

interface SentResize {
  size: TerminalSize;
  generation: DecimalU64;
}

/** Codes of `error{attachment}` that are not failures: the owner re-sends after the next ownership. */
const IGNORED_ATTACHMENT_ERRORS = new Set(['stale_generation', 'unknown_attachment']);

/** The daemon's refusal of an attach or resume that names another backend instance. */
const BACKEND_MISMATCH = 'backend_mismatch';
const BACKEND_GONE: PanelStatus = { kind: 'gone', reason: 'backendGone' };

export class AttachmentMachine {
  private visible = false;
  private focused = false;
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
  private sentDisplay: Display | null = null;
  private sentResize: SentResize | null = null;
  private claimedGeneration: DecimalU64 | null = null;
  private effects: MachineEffect[] = [];

  /**
   * `backendInstance` is the backend instance a pinned panel's terminal belongs to, or `null` for a
   * panel that follows the active terminal. A pinned panel counts as connected only while the
   * session's daemon serves that instance, so it never attaches or resumes on another core's
   * terminal that reuses the ID; its attach and resume also name the instance, for the daemon
   * to check.
   */
  constructor(
    private readonly terminal: TermId,
    private readonly backendInstance: string | null = null,
  ) {}

  handle(input: MachineInput): MachineEffect[] {
    this.effects = [];
    this.step(input);
    this.advance();
    this.maybeResume();
    this.maybeClaim();
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
      canRetry: this.status?.kind === 'streamFailed',
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

  /** The attachment is named, connected and past the attach: display facts may be sent. */
  private canSendDisplay(): boolean {
    return (
      this.attachment !== null &&
      this.connected &&
      (this.phase === 'live' || this.phase === 'attaching')
    );
  }

  private displayChanged(): boolean {
    return this.sentDisplay === null || !sameDisplay(this.display(), this.sentDisplay);
  }

  private checkDisplay(): void {
    if (this.canSendDisplay() && this.displayChanged()) {
      this.effects.push({ kind: 'startTimer', timer: 'display', ms: DISPLAY_DEBOUNCE_MS });
    }
  }

  private startResizeTimer(): void {
    if (this.phase === 'live' && this.isOwner()) {
      this.effects.push({ kind: 'startTimer', timer: 'resize', ms: RESIZE_DEBOUNCE_MS });
    }
  }

  /** True when bytes from xterm may leave: a live, connected attachment with a usable screen. */
  private canSendBytes(): boolean {
    return (
      this.phase === 'live' &&
      this.connected &&
      this.attachment !== null &&
      this.instance !== null &&
      !this.stale
    );
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
        this.startResizeTimer();
        return;
      case 'cellSize':
        this.cell = input.cell;
        this.checkDisplay();
        this.startResizeTimer();
        return;
      case 'timer':
        switch (input.timer) {
          case 'ack':
            return this.onAckTimer();
          case 'resize':
            return this.onResizeTimer();
          case 'display':
            return this.onDisplayTimer();
        }
        return;
      case 'reattach':
        return this.reattach();
      case 'userInput':
        return this.onUserInput(input.bytes);
      case 'automaticReply':
        return this.onAutomaticReply(input.bytes);
      case 'focusReport':
        return this.onFocusReport(input.bytes);
      case 'domFocus':
        return this.onDomFocus();
      case 'takeInput':
        return this.onTakeInput();
      case 'focused':
        this.focused = input.focused;
        return;
      case 'streamFailed':
        this.status = { kind: 'streamFailed', message: input.message };
        return;
      case 'retryStream':
        if (this.status?.kind === 'streamFailed') this.status = null;
        return;
    }
  }

  private onUserInput(bytes: Uint8Array): void {
    if (this.canSendBytes() && this.attachment !== null) {
      this.effects.push({ kind: 'input', attachment: this.attachment, bytes });
    }
  }

  private onFocusReport(bytes: Uint8Array): void {
    if (this.isOwner()) this.onUserInput(bytes);
  }

  private onAutomaticReply(bytes: Uint8Array): void {
    const applying = this.applying;
    if (
      this.phase !== 'live' ||
      !this.connected ||
      this.attachment === null ||
      this.ownership === null ||
      applying === null ||
      applying.type !== 'data' ||
      this.isReplay(applying.cursor) ||
      !this.isOwner()
    ) {
      return;
    }
    this.effects.push({
      kind: 'automaticReply',
      attachment: this.attachment,
      epoch: applying.cursor.epoch,
      ownershipGeneration: this.ownership.generation,
      bytes,
    });
  }

  private onDomFocus(): void {
    if (this.phase === 'live' && this.connected && this.attachment !== null) {
      this.effects.push({ kind: 'focus', attachment: this.attachment });
    }
  }

  private onTakeInput(): void {
    if (this.phase === 'live' && this.connected && this.attachment !== null && !this.isOwner()) {
      this.requestInput(this.attachment, null);
    }
  }

  /**
   * Sends the fit as this attachment's resize first, so the daemon stores it and applies it on
   * the grant, then asks for input. `ifVacant` makes the request conditional (see the effect).
   */
  private requestInput(attachment: DecimalU64, ifVacant: DecimalU64 | null): void {
    if (this.ownership !== null && this.container !== null) {
      const fit = fitGrid(this.container, this.cell);
      if (fit !== null) {
        const generation = this.ownership.generation;
        this.sentResize = { size: fit, generation };
        this.effects.push({
          kind: 'resize',
          attachment,
          size: fit,
          ownershipGeneration: generation,
        });
      }
    }
    this.effects.push({ kind: 'requestOwnership', attachment, ifVacant });
  }

  /**
   * A focused live panel claims a terminal that has no owner, once per ownership generation. The
   * claim names that generation, so it can't take ownership from a viewer that acquired it since.
   */
  private maybeClaim(): void {
    if (
      !this.visible ||
      !this.focused ||
      this.phase !== 'live' ||
      !this.connected ||
      this.attachment === null ||
      this.instance === null ||
      this.stale ||
      this.ownership === null ||
      this.ownership.owner !== null ||
      this.claimedGeneration === this.ownership.generation
    ) {
      return;
    }
    this.claimedGeneration = this.ownership.generation;
    this.requestInput(this.attachment, this.ownership.generation);
  }

  private onResizeTimer(): void {
    if (
      this.phase !== 'live' ||
      !this.connected ||
      this.attachment === null ||
      this.ownership === null ||
      !this.isOwner() ||
      this.instance === null ||
      this.container === null
    ) {
      return;
    }
    const fit = fitGrid(this.container, this.cell);
    if (fit === null || sameSize(fit, this.instance.size)) return;
    const generation = this.ownership.generation;
    const sent = this.sentResize;
    if (sent !== null && sent.generation === generation && sameSize(sent.size, fit)) return;
    this.sentResize = { size: fit, generation };
    this.effects.push({
      kind: 'resize',
      attachment: this.attachment,
      size: fit,
      ownershipGeneration: generation,
    });
  }

  private onDisplayTimer(): void {
    if (this.attachment === null || !this.canSendDisplay() || !this.displayChanged()) return;
    const display = this.display();
    this.sentDisplay = display;
    this.effects.push({ kind: 'setDisplay', attachment: this.attachment, display });
  }

  private show(): void {
    if (this.visible) return;
    this.visible = true;
    // A stream failure stays until Retry, and a session of another backend instance until one
    // of the pinned instance connects.
    const kept = this.status?.kind === 'streamFailed' || (this.status?.kind === 'gone' && !this.connected);
    if (!kept) this.status = null;
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
        return this.onConnection(event.connected, event.instance);
      case 'attached':
        return this.onAttached(event.attachment);
      case 'gap':
        if (event.request !== null) this.onRequestAnswerFailed();
        else this.onGap(event.attachment);
        return;
      case 'error':
        if (event.request !== null) this.onRequestAnswerFailed(event.code);
        else if (event.attachment !== null) this.onAttachmentError(event.attachment, event.code);
        return;
      case 'resumed':
        return this.onResumed(event.attachment, event.replay_through);
      case 'ownership':
        return this.onOwnership(event.attachment, event.ownership_generation, event.owner);
      default:
        this.onStreamEvent(event);
    }
  }

  /** A pinned panel treats a session that serves another backend instance as no connection. */
  private onConnection(connected: boolean, instance: string | null): void {
    const otherInstance =
      connected && this.backendInstance !== null && instance !== this.backendInstance;
    if (!connected || otherInstance) {
      this.onDisconnected();
      if (otherInstance) this.status = BACKEND_GONE;
      return;
    }
    if (this.status?.kind === 'gone') {
      this.status = null;
      if (this.phase === 'ended') this.phase = this.visible ? 'waiting' : 'hidden';
    }
    this.onConnected();
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
      this.checkDisplay();
    }
  }

  private onOwnership(
    attachment: DecimalU64,
    generation: DecimalU64,
    owner: DecimalU64 | null,
  ): void {
    if (attachment !== this.attachment) return;
    if (this.phase !== 'attaching' && this.phase !== 'live') return;
    this.ownership = { generation, owner };
    if (this.status?.kind === 'error') this.status = null;
    this.startResizeTimer();
  }

  private onAttachmentError(attachment: DecimalU64, code: string): void {
    if (this.attachment === null || attachment !== this.attachment) return;
    if (this.phase !== 'attaching' && this.phase !== 'live') return;
    if (IGNORED_ATTACHMENT_ERRORS.has(code)) return;
    this.status = { kind: 'error', code };
  }

  private onResumed(attachment: DecimalU64, replayThrough: Cursor | null): void {
    if (this.phase === 'resuming' && attachment === this.attachment) {
      this.phase = 'live';
      this.replayThrough = replayThrough;
      this.checkDisplay();
    } else if (this.phase === 'detaching' && this.detachWhenNamed) {
      this.startDetach(attachment);
    }
  }

  /** `gap{request}` and `error{request}`, which answer an attach or a resume. */
  private onRequestAnswerFailed(code?: string): void {
    if (code === BACKEND_MISMATCH && (this.phase === 'resuming' || this.phase === 'attaching')) {
      // The daemon serves another backend instance: the pinned terminal is gone from it. The
      // screen stays, dimmed, and nothing is sent until a session of the right instance returns.
      this.cancelTimer('ack');
      this.ackScheduled = false;
      this.resetStream();
      this.attachment = null;
      this.stale = this.instance !== null;
      this.status = BACKEND_GONE;
      this.phase = 'ended';
    } else if (this.phase === 'detaching' && this.detachWhenNamed) {
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
    const wasStarted = this.applying?.type === 'started';
    this.applying = null;
    this.applied = cursor;
    if (this.replayThrough !== null && !cursorAfter(this.replayThrough, cursor)) {
      this.replayThrough = null;
    }
    if (this.connected && this.phase === 'live' && !this.ackScheduled) {
      this.effects.push({ kind: 'startTimer', timer: 'ack', ms: ACK_INTERVAL_MS });
      this.ackScheduled = true;
    }
    if (wasStarted) this.startResizeTimer();
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
    this.effects.push({ kind: 'resume', cursor: this.applied, instance: this.backendInstance });
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
      instance: this.backendInstance,
    });
    this.sentDisplay = display;
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
    this.sentResize = null;
    this.claimedGeneration = null;
    this.queue.clear();
    this.applying = null;
  }

  private cancelTimer(timer: TimerName): void {
    this.effects.push({ kind: 'cancelTimer', timer });
  }
}
