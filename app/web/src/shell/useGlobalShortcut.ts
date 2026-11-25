import { useLayoutEffect } from 'react';

export function useGlobalShortcut(key: string, onTrigger: () => void): void {
  useLayoutEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (!event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
      if (event.key.toLowerCase() !== key && event.code !== `Key${key.toUpperCase()}`) return;
      event.preventDefault();
      onTrigger();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [key, onTrigger]);
}
