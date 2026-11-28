import { useEffect, useRef } from 'react';
import type { KeyboardEvent } from 'react';

interface ConfirmClosePanelProps {
  label: string;
  onConfirm: () => void;
  onCancel: () => void;
}

/** Asks before a scratch panel is closed, since closing deletes its content. */
export function ConfirmClosePanel({ label, onConfirm, onCancel }: ConfirmClosePanelProps) {
  const cancel = useRef<HTMLButtonElement>(null);
  const confirm = useRef<HTMLButtonElement>(null);

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
        <h2 id="confirm-close-title">Close scratch panel?</h2>
        <p id="confirm-close-text">
          {`Closing ${label} deletes its content, and it can't be restored. Pin it first to keep it in the project.`}
        </p>
        <div className="confirm-actions">
          <button ref={cancel} type="button" onClick={onCancel}>
            Cancel
          </button>
          <button ref={confirm} type="button" className="button-danger" onClick={onConfirm}>
            Close and delete
          </button>
        </div>
      </div>
    </div>
  );
}
