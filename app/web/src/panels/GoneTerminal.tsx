import './terminalPanel.css';

export interface GoneTerminalProps {
  message: string;
  onClose: () => void;
  disabledTitle?: string;
}

/** Shows why a pinned terminal can't be shown. It attaches nothing. */
export function GoneTerminal({ message, onClose, disabledTitle }: GoneTerminalProps) {
  return (
    <section className="terminal-panel" data-testid="gone-terminal">
      <div className="terminal-panel__status">
        <span data-testid="gone-terminal-message">{message}</span>
        <button
          type="button"
          disabled={disabledTitle !== undefined}
          title={disabledTitle}
          onClick={onClose}
        >
          Close
        </button>
      </div>
    </section>
  );
}
