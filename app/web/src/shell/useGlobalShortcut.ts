import { useEffect } from 'react';

export function useGlobalShortcut(key: string, onTrigger: () => void): void {
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (!event.metaKey || event.key.toLowerCase() !== key) return;
      event.preventDefault();
      onTrigger();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [key, onTrigger]);
}
