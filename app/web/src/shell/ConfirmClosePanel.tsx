import { useEffect, useRef } from 'react';
import type { KeyboardEvent } from 'react';
import type { PanelLifetime } from '../model/panels';

interface ConfirmClosePanelProps {
  label: string;
  lifetime: PanelLifetime | null;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmClosePanel({ label, lifetime, onConfirm, onCancel }: ConfirmClosePanelProps) {
  const cancel = useRef<HTMLButtonElement>(null);
  const confirm = useRef<HTMLButtonElement>(null);
  const scratch = lifetime === 'scratch';

  useEffect(() => {
    const previous = document.activeElement;
    cancel.current?.focus();
    return () => {
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);

  function onKeyDown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      event.preventDefault();
      onCancel();
      return;
    }
    if (event.key !== 'Tab') return;
    event.preventDefault();
    const other = document.activeElement === cancel.current ? confirm.current : cancel.current;
    other?.focus();
  }

  return (
    <div
      className="confirm-backdrop"
      onKeyDown={onKeyDown}
      onClick={(event) => {
        if (event.target === event.currentTarget) onCancel();
      }}
    >
      <div
        className="confirm-close"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="confirm-close-title"
        aria-describedby="confirm-close-text"
      >
        <h2 id="confirm-close-title">{scratch ? 'Close scratch panel?' : 'Close panel?'}</h2>
        <p id="confirm-close-text">
          {scratch
            ? `Closing ${label} deletes its content, and it can't be restored. Pin it first to keep it in the project.`
            : `Closing ${label} may delete its content, and it can't be restored.`}
        </p>
        <div className="confirm-actions">
          <button ref={cancel} type="button" onClick={onCancel}>
            Cancel
          </button>
          <button ref={confirm} type="button" onClick={onConfirm}>
            {scratch ? 'Close and delete' : 'Close'}
          </button>
        </div>
      </div>
    </div>
  );
}
