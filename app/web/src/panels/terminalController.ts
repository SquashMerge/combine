import type { Cursor, HostId, TermId } from '../model/ids';
import { sameSize, type TerminalSize, type TerminalStreamEvent } from '../model/terminal';
import type { TerminalPort, TerminalStream } from '../ipc/terminalPort';
import type {
  CellSize,
  XtermAdapter,
  XtermAdapterFactory,
  XtermCallbacks,
} from '../renderers/terminal/adapter';
import {
  AttachmentMachine,
  type MachineEffect,
  type MachineInput,
  type PanelView,
  type TimerName,
} from './attachmentMachine';

export interface TerminalControllerOptions {
  port: TerminalPort;
  hostId: HostId;
  termId: TermId;
  createAdapter: XtermAdapterFactory;
}

function sameCell(a: CellSize, b: CellSize): boolean {
  return a.width === b.width && a.height === b.height;
}

function sameStatus(a: PanelView['status'], b: PanelView['status']): boolean {
  if (a === null || b === null) return a === b;
  return a.kind === b.kind && (a.kind !== 'error' || (b.kind === 'error' && a.code === b.code));
}

function sameView(a: PanelView, b: PanelView): boolean {
  return (
    a.phase === b.phase &&
    a.connected === b.connected &&
    a.ownership === b.ownership &&
    (a.ptySize === null || b.ptySize === null
      ? a.ptySize === b.ptySize
      : sameSize(a.ptySize, b.ptySize)) &&
    sameCell(a.cell, b.cell) &&
    a.letterboxed === b.letterboxed &&
    a.stale === b.stale &&
    a.hasInstance === b.hasInstance &&
    sameStatus(a.status, b.status) &&
    a.fidelity === b.fidelity &&
    a.outputSkipped === b.outputSkipped &&
    a.unorderedResizes === b.unorderedResizes &&
    a.canTakeInput === b.canTakeInput &&
    a.canReattach === b.canReattach
  );
}

/** Runs the machine's effects against a terminal stream and an xterm adapter. */
export class TerminalController {
  private readonly machine: AttachmentMachine;
  private readonly listeners = new Set<() => void>();
  private readonly timers = new Map<TimerName, ReturnType<typeof setTimeout>>();
  private readonly inFlight = new Set<Promise<void>>();
  private readonly inputs: MachineInput[] = [];
  private readonly early: TerminalStreamEvent[] = [];
  private view: PanelView;
  private stream: TerminalStream | null = null;
  private host: HTMLElement | null = null;
  private adapter: XtermAdapter | null = null;
  private instanceToken = 0;
  private requestSeq = 0;
  private pendingSeq: number | null = null;
  private draining = false;
  private stopped = false;
  private stopping: Promise<void> | null = null;
  private onHidden: (() => void) | null = null;

  constructor(private readonly options: TerminalControllerOptions) {
    this.machine = new AttachmentMachine(options.termId);
    this.view = this.machine.view();
  }

  async start(host: HTMLElement): Promise<void> {
    this.host = host;
    const stream = await this.options.port.openStream(this.options.hostId, (event) => {
      if (this.stream === null) this.early.push(event);
      else this.dispatch({ kind: 'event', event });
    });
    if (this.stopped) {
      await stream.close();
      return;
    }
    this.stream = stream;
    for (const event of this.early.splice(0)) this.dispatch({ kind: 'event', event });
  }

  /**
   * Hides the panel, which detaches from the webview, then closes the panel stream once
   * the detach has finished and the final event's ack has been accepted or rejected.
   */
  stop(): Promise<void> {
    this.stopping ??= this.shutDown();
    return this.stopping;
  }

  setVisible(visible: boolean): void {
    this.input({ kind: visible ? 'show' : 'hide' });
  }

  setContainerSize(width: number, height: number): void {
    this.input({ kind: 'containerSize', width, height });
  }

  takeInput(): void {
    this.input({ kind: 'takeInput' });
  }

  reattach(): void {
    this.input({ kind: 'reattach' });
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  getView(): PanelView {
    return this.view;
  }

  private async shutDown(): Promise<void> {
    this.stopped = true;
    this.dispatch({ kind: 'hide' });
    if (this.view.phase !== 'hidden') {
      await new Promise<void>((resolve) => {
        this.onHidden = resolve;
      });
    }
    await Promise.allSettled([...this.inFlight]);
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
    await this.stream?.close();
  }

  /** UI input is ignored once the controller is stopping. */
  private input(input: MachineInput): void {
    if (!this.stopped) this.dispatch(input);
  }

  private dispatch(input: MachineInput): void {
    this.inputs.push(input);
    if (this.draining) return;
    this.draining = true;
    try {
      for (let next = this.inputs.shift(); next !== undefined; next = this.inputs.shift()) {
        for (const effect of this.machine.handle(next)) this.run(effect);
        this.refresh();
      }
    } finally {
      this.draining = false;
    }
  }

  private refresh(): void {
    const next = this.machine.view();
    if (!sameView(this.view, next)) {
      this.view = next;
      for (const listener of [...this.listeners]) listener();
    }
    if (next.phase === 'hidden' && this.onHidden !== null) {
      const resolve = this.onHidden;
      this.onHidden = null;
      resolve();
    }
  }

  private run(effect: MachineEffect): void {
    const stream = this.stream;
    switch (effect.kind) {
      case 'attach':
        return this.request('attach', stream?.attach(effect.terminal, effect.options));
      case 'resume':
        return this.request('resume', stream?.resume(effect.cursor));
      case 'detach':
        return this.send(stream?.detach(effect.attachment));
      case 'ack':
        return this.send(stream?.ack(effect.cursor));
      case 'input':
        return this.send(stream?.input(effect.attachment, effect.bytes));
      case 'automaticReply':
        return this.send(
          stream?.automaticReply(
            effect.attachment,
            effect.epoch,
            effect.ownershipGeneration,
            effect.bytes,
          ),
        );
      case 'resize':
        return this.send(
          stream?.resize(effect.attachment, effect.size, effect.ownershipGeneration),
        );
      case 'focus':
        return this.send(stream?.focus(effect.attachment));
      case 'setDisplay':
        return this.send(stream?.setDisplay(effect.attachment, effect.display));
      case 'requestOwnership':
        return this.send(stream?.requestOwnership(effect.attachment));
      case 'createInstance':
        return this.createInstance(effect.size, effect.cursor);
      case 'disposeInstance':
        this.instanceToken += 1;
        this.adapter?.dispose();
        this.adapter = null;
        return;
      case 'write': {
        const token = this.instanceToken;
        this.adapter?.write(effect.bytes, () => {
          if (token === this.instanceToken) this.dispatch({ kind: 'applied', cursor: effect.cursor });
        });
        return;
      }
      case 'resizeInstance':
        this.adapter?.resize(effect.size);
        this.dispatch({ kind: 'applied', cursor: effect.cursor });
        return;
      case 'startTimer':
        this.startTimer(effect.timer, effect.ms);
        return;
      case 'cancelTimer':
        this.clearTimer(effect.timer);
        return;
    }
  }

  /**
   * Sends an attach or resume. Each one is numbered, and only the outstanding one's rejection
   * counts: a late rejection of an earlier request must not undo a newer attach.
   */
  private request(
    op: 'attach' | 'resume',
    result: Promise<void> | undefined,
  ): void {
    if (result === undefined) return;
    this.requestSeq += 1;
    const seq = this.requestSeq;
    this.pendingSeq = seq;
    result.catch((error: unknown) => {
      if (seq !== this.pendingSeq) return;
      this.dispatch({
        kind: 'sendFailed',
        op,
        notConnected: error instanceof Error && error.message === 'not connected',
      });
    });
  }

  /** Other commands' rejections are ignored; the connection event reports a drop. */
  private send(result: Promise<void> | undefined): void {
    if (result === undefined) return;
    const settled = result.then(
      () => undefined,
      () => undefined,
    );
    this.inFlight.add(settled);
    void settled.then(() => this.inFlight.delete(settled));
  }

  private createInstance(size: TerminalSize, cursor: Cursor): void {
    this.instanceToken += 1;
    const token = this.instanceToken;
    queueMicrotask(() => {
      if (token !== this.instanceToken || this.stopped || this.host === null) return;
      const hadFocus = this.adapter?.hasFocus() ?? false;
      this.adapter?.dispose();
      this.adapter = this.options.createAdapter(this.host, size, this.callbacks(token));
      if (hadFocus) this.adapter.focus();
      this.dispatch({ kind: 'applied', cursor });
    });
  }

  private callbacks(token: number): XtermCallbacks {
    const forward = (input: MachineInput): void => {
      if (token === this.instanceToken) this.input(input);
    };
    return {
      onInput: (bytes) => forward({ kind: 'userInput', bytes }),
      onAutomaticReply: (bytes) => forward({ kind: 'automaticReply', bytes }),
      onFocusReport: (bytes) => forward({ kind: 'focusReport', bytes }),
      onFocus: () => forward({ kind: 'domFocus' }),
      onCellSize: (cell) => forward({ kind: 'cellSize', cell }),
    };
  }

  private startTimer(timer: TimerName, ms: number): void {
    this.clearTimer(timer);
    this.timers.set(
      timer,
      setTimeout(() => {
        this.timers.delete(timer);
        this.dispatch({ kind: 'timer', timer });
      }, ms),
    );
  }

  private clearTimer(timer: TimerName): void {
    const pending = this.timers.get(timer);
    if (pending === undefined) return;
    clearTimeout(pending);
    this.timers.delete(timer);
  }
}
