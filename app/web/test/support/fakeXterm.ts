import type {
  CellSize,
  XtermAdapter,
  XtermAdapterFactory,
  XtermCallbacks,
} from '../../src/renderers/terminal/adapter';
import type { TerminalSize } from '../../src/model/terminal';

export interface FakeWrite {
  bytes: Uint8Array;
  done: () => void;
  completed: boolean;
}

const encoder = new TextEncoder();

export class FakeXterm implements XtermAdapter {
  readonly renderer = 'dom' as const;
  readonly writes: FakeWrite[] = [];
  readonly resizes: TerminalSize[] = [];
  size: TerminalSize;
  disposed = false;
  focused = false;

  constructor(
    readonly host: HTMLElement,
    size: TerminalSize,
    readonly callbacks: XtermCallbacks,
    private readonly log: string[],
    readonly index: number,
  ) {
    this.size = size;
    this.log.push(`create ${index}`);
  }

  write(bytes: Uint8Array, done: () => void): void {
    this.writes.push({ bytes, done, completed: false });
  }

  resize(size: TerminalSize): void {
    this.size = size;
    this.resizes.push(size);
  }

  focus(): void {
    this.focused = true;
  }

  hasFocus(): boolean {
    return this.focused;
  }

  dispose(): void {
    this.disposed = true;
    this.log.push(`dispose ${this.index}`);
  }

  /** Writes issued and not yet completed. */
  pendingWrites(): FakeWrite[] {
    return this.writes.filter((write) => !write.completed);
  }

  /** Runs the oldest outstanding write's done callback. Returns false when none is outstanding. */
  completeWrite(): boolean {
    const write = this.pendingWrites()[0];
    if (write === undefined) return false;
    write.completed = true;
    write.done();
    return true;
  }

  /** Completes writes, including ones issued from inside a done callback, until none is outstanding. */
  completeAll(): void {
    while (this.completeWrite()) {
      // Each completion may issue the next write.
    }
  }

  emitInput(text: string): void {
    this.callbacks.onInput(encoder.encode(text));
  }

  emitAutomaticReply(text: string): void {
    this.callbacks.onAutomaticReply(encoder.encode(text));
  }

  emitFocusReport(text: string): void {
    this.callbacks.onFocusReport(encoder.encode(text));
  }

  emitFocus(): void {
    this.focused = true;
    this.callbacks.onFocus();
  }

  emitCellSize(cell: CellSize): void {
    this.callbacks.onCellSize(cell);
  }
}

export interface FakeXtermFactory {
  factory: XtermAdapterFactory;
  instances: FakeXterm[];
  /** `create N` and `dispose N`, in the order they happened. */
  log: string[];
}

export function fakeXtermFactory(): FakeXtermFactory {
  const instances: FakeXterm[] = [];
  const log: string[] = [];
  const factory: XtermAdapterFactory = (host, size, callbacks) => {
    const instance = new FakeXterm(host, size, callbacks, log, instances.length);
    instances.push(instance);
    return instance;
  };
  return { factory, instances, log };
}
