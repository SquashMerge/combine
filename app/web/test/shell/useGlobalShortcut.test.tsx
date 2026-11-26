import { fireEvent, render } from '@testing-library/react';
import { useLayoutEffect } from 'react';
import { describe, expect, it, vi } from 'vitest';
import { useGlobalShortcut } from '../../src/shell/useGlobalShortcut';

function Probe({ onTrigger }: { onTrigger: () => void }) {
  useGlobalShortcut('k', onTrigger);
  return null;
}

describe('useGlobalShortcut', () => {
  it('registers before the first paint', () => {
    const onTrigger = vi.fn();
    let fired = false;
    // A sibling's layout effect runs in the same commit, before any paint or passive effect.
    function Late() {
      useLayoutEffect(() => {
        fired = !fireEvent.keyDown(window, { key: 'k', metaKey: true });
      }, []);
      return null;
    }
    function Early() {
      useGlobalShortcut('k', onTrigger);
      return null;
    }
    render(
      <>
        <Early />
        <Late />
      </>,
    );
    expect(fired).toBe(true);
    expect(onTrigger).toHaveBeenCalledOnce();
  });

  it('opens on Cmd+K and prevents the default', () => {
    const onTrigger = vi.fn();
    render(<Probe onTrigger={onTrigger} />);
    expect(fireEvent.keyDown(window, { key: 'K', code: 'KeyK', metaKey: true })).toBe(false);
    expect(onTrigger).toHaveBeenCalledOnce();
  });

  it('ignores Cmd+K with Ctrl, Alt or Shift', () => {
    const onTrigger = vi.fn();
    render(<Probe onTrigger={onTrigger} />);
    for (const extra of [{ ctrlKey: true }, { altKey: true }, { shiftKey: true }]) {
      fireEvent.keyDown(window, { key: 'k', code: 'KeyK', metaKey: true, ...extra });
    }
    fireEvent.keyDown(window, { key: 'k', code: 'KeyK' });
    expect(onTrigger).not.toHaveBeenCalled();
  });

  it('matches by code in a non-Latin layout', () => {
    const onTrigger = vi.fn();
    render(<Probe onTrigger={onTrigger} />);
    fireEvent.keyDown(window, { key: 'л', code: 'KeyK', metaKey: true });
    expect(onTrigger).toHaveBeenCalledOnce();
  });

  it('does not open on Cmd+J', () => {
    const onTrigger = vi.fn();
    render(<Probe onTrigger={onTrigger} />);
    fireEvent.keyDown(window, { key: 'j', code: 'KeyJ', metaKey: true });
    expect(onTrigger).not.toHaveBeenCalled();
  });

  it('unbinds on unmount', () => {
    const onTrigger = vi.fn();
    const { unmount } = render(<Probe onTrigger={onTrigger} />);
    unmount();
    fireEvent.keyDown(window, { key: 'k', metaKey: true });
    expect(onTrigger).not.toHaveBeenCalled();
  });
});
