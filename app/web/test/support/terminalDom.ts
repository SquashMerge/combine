type ResizeCallback = (entries: ResizeObserverEntry[], observer: ResizeObserver) => void;

const observers = new Set<FakeResizeObserver>();

export class FakeResizeObserver implements ResizeObserver {
  private readonly targets = new Set<Element>();

  constructor(private readonly callback: ResizeCallback) {
    observers.add(this);
  }

  observe(target: Element): void {
    this.targets.add(target);
  }

  unobserve(target: Element): void {
    this.targets.delete(target);
  }

  disconnect(): void {
    this.targets.clear();
    observers.delete(this);
  }

  static resize(el: Element, width: number, height: number): void {
    const contentRect = {
      x: 0,
      y: 0,
      top: 0,
      left: 0,
      width,
      height,
      right: width,
      bottom: height,
      toJSON: () => ({}),
    } as DOMRectReadOnly;
    for (const observer of [...observers]) {
      if (!observer.targets.has(el)) continue;
      const entry = { target: el, contentRect } as ResizeObserverEntry;
      observer.callback([entry], observer);
    }
  }
}

export function installTerminalDom(): void {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    writable: true,
    value: (media: string): MediaQueryList => ({
      matches: false,
      media,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    }),
  });
  HTMLCanvasElement.prototype.getContext = () => null;
  globalThis.ResizeObserver = FakeResizeObserver;
}
