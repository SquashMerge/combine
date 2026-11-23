import { render } from '@testing-library/react';
import { beforeAll, describe, expect, it, vi } from 'vitest';
import { TerminalView, type TerminalViewProps } from '../../src/renderers/terminal/TerminalView';
import { LETTERBOX_BACKGROUND, TERMINAL_THEME } from '../../src/renderers/terminal/settings';
import { FakeResizeObserver, installTerminalDom } from '../support/terminalDom';

function props(overrides: Partial<TerminalViewProps> = {}): TerminalViewProps {
  return {
    ptySize: { cols: 80, rows: 24 },
    cell: { width: 8, height: 17 },
    letterboxed: false,
    dimmed: false,
    overlay: null,
    hostRef: () => {},
    onContainerSize: () => {},
    ...overrides,
  };
}

describe('TerminalView', () => {
  beforeAll(() => {
    installTerminalDom();
  });

  it('sizes the viewport to the PTY size in cell pixels', () => {
    const { container } = render(<TerminalView {...props({ cell: { width: 8.5, height: 17 } })} />);
    const viewport = container.querySelector<HTMLElement>('.terminal-view__viewport');
    expect(viewport?.style.width).toBe('680px');
    expect(viewport?.style.height).toBe('408px');
    expect(viewport?.dataset.cols).toBe('80');
    expect(viewport?.dataset.rows).toBe('24');
    const view = container.querySelector<HTMLElement>('.terminal-view');
    expect(view?.dataset.letterboxed).toBe('false');
    expect(view?.style.background).not.toBe('');
    expect(view?.style.backgroundColor).toBe(hex(TERMINAL_THEME.background));
  });

  it('marks a non-owner view letterboxed', () => {
    const { container } = render(<TerminalView {...props({ letterboxed: true })} />);
    const view = container.querySelector<HTMLElement>('.terminal-view');
    expect(view?.dataset.letterboxed).toBe('true');
    expect(view?.style.backgroundColor).toBe(hex(LETTERBOX_BACKGROUND));
  });

  it('dims the viewport and shows the overlay text', () => {
    const { container, getByRole } = render(
      <TerminalView {...props({ dimmed: true, overlay: 'Reconnecting' })} />,
    );
    expect(container.querySelector<HTMLElement>('.terminal-view__viewport')?.dataset.dimmed).toBe('true');
    expect(getByRole('status').textContent).toBe('Reconnecting');
  });

  it('reports the container size from ResizeObserver', () => {
    const onContainerSize = vi.fn();
    const { container } = render(<TerminalView {...props({ onContainerSize })} />);
    const view = container.querySelector<HTMLElement>('.terminal-view') as HTMLElement;
    FakeResizeObserver.resize(view, 640, 480);
    expect(onContainerSize).toHaveBeenCalledWith({ width: 640, height: 480 });
  });
});

function hex(color: string): string {
  const n = parseInt(color.slice(1), 16);
  return `rgb(${n >> 16}, ${(n >> 8) & 255}, ${n & 255})`;
}
