import { useEffect, useRef } from 'react';
import type { TerminalSize } from '../../model/terminal';
import type { CellSize } from './adapter';
import { viewportPixels } from './geometry';
import { LETTERBOX_BACKGROUND, TERMINAL_THEME } from './settings';
import './terminalView.css';

export interface TerminalViewProps {
  ptySize: TerminalSize | null;
  cell: CellSize;
  letterboxed: boolean;
  dimmed: boolean;
  overlay: string | null;
  /** Where the controller mounts instances. */
  hostRef: (element: HTMLDivElement | null) => void;
  /** From a ResizeObserver on the container (contentRect). */
  onContainerSize: (size: { width: number; height: number }) => void;
}

export function TerminalView({
  ptySize,
  cell,
  letterboxed,
  dimmed,
  overlay,
  hostRef,
  onContainerSize,
}: TerminalViewProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const onSizeRef = useRef(onContainerSize);
  onSizeRef.current = onContainerSize;

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[entries.length - 1];
      if (!entry) return;
      const { width, height } = entry.contentRect;
      onSizeRef.current({ width, height });
    });
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  const pixels = ptySize ? viewportPixels(ptySize, cell) : null;
  return (
    <div
      ref={containerRef}
      className="terminal-view"
      data-letterboxed={letterboxed ? 'true' : 'false'}
      style={{ background: letterboxed ? LETTERBOX_BACKGROUND : TERMINAL_THEME.background }}
    >
      <div
        ref={hostRef}
        className="terminal-view__viewport"
        data-dimmed={dimmed ? 'true' : 'false'}
        data-cols={ptySize?.cols}
        data-rows={ptySize?.rows}
        style={pixels ? { width: pixels.width, height: pixels.height } : undefined}
      />
      {overlay !== null && (
        <div className="terminal-view__overlay" role="status">
          {overlay}
        </div>
      )}
    </div>
  );
}
