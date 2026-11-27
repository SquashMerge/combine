import type { HostId, TermId } from '../model/ids';
import type { TerminalPort } from '../ipc/terminalPort';
import type { XtermAdapterFactory } from '../renderers/terminal/adapter';
import { TerminalView } from '../renderers/terminal/TerminalView';
import { createXtermAdapter } from '../renderers/terminal/xtermAdapter';
import { badges, goneText, overlayText, ownershipText, statusText } from './panelText';
import { useTerminalPanel } from './useTerminalPanel';
import './terminalPanel.css';

export interface TerminalSwitcher {
  options: { id: string; label: string; disabled: boolean }[];
  onSwitch: (id: string) => void;
  /** Shown as the tooltip of each disabled option. */
  disabledTitle?: string;
}

export interface TerminalPanelProps {
  port: TerminalPort;
  hostId: HostId;
  termId: TermId;
  /** The backend instance a pinned panel's terminal belongs to; absent or `null` to follow the active terminal. */
  instance?: string | null;
  visible: boolean;
  label: { name: string; host: string; state: string; backend?: string | null };
  focused?: boolean;
  switcher?: TerminalSwitcher;
  onClose?: () => void;
  closeDisabledTitle?: string;
  createAdapter?: XtermAdapterFactory;
}

/** `hostId`, `termId` and `instance` are fixed for the panel's lifetime: key the panel by all three. */
export function TerminalPanel({
  port,
  hostId,
  termId,
  instance = null,
  visible,
  label,
  focused = false,
  switcher,
  onClose,
  closeDisabledTitle,
  createAdapter = createXtermAdapter,
}: TerminalPanelProps) {
  const { view, hostRef, onContainerSize, takeInput, reattach, retry } = useTerminalPanel({
    port,
    hostId,
    termId,
    instance,
    visible,
    focused,
    createAdapter,
  });
  const status = view.status?.kind === 'gone' ? goneText(view.status.reason, termId) : statusText(view);
  const canClose =
    onClose !== undefined &&
    (label.state === 'exited' || view.status?.kind === 'exited' || view.status?.kind === 'gone');

  return (
    <section className="terminal-panel" hidden={!visible} data-testid="terminal-panel">
      <header className="terminal-panel__header">
        <div className="terminal-panel__title">
          <span className="terminal-panel__name">{label.name}</span>
          <span className="terminal-panel__state">{label.state}</span>
          <span className="terminal-panel__host">{label.host}</span>
          {label.backend != null && (
            <span className="terminal-panel__backend" data-testid="terminal-backend">
              {label.backend}
            </span>
          )}
          {switcher !== undefined && (
            <select
              className="terminal-panel__switcher"
              aria-label="Terminal"
              data-testid="terminal-switcher"
              value={termId}
              onChange={(event) => switcher.onSwitch(event.target.value)}
            >
              {switcher.options.map((option) => (
                <option
                  key={option.id}
                  value={option.id}
                  disabled={option.disabled}
                  title={option.disabled ? switcher.disabledTitle : undefined}
                >
                  {option.label}
                </option>
              ))}
            </select>
          )}
        </div>
        <div className="terminal-panel__meta">
          <span className="terminal-panel__ownership">{ownershipText(view.ownership)}</span>
          {badges(view).map((badge) => (
            <span key={badge} className="terminal-panel__badge">
              {badge}
            </span>
          ))}
          <button type="button" disabled={!view.canTakeInput} onClick={takeInput}>
            Take input
          </button>
        </div>
        {(status !== null || view.canReattach || view.canRetry || canClose) && (
          <div className="terminal-panel__status">
            {status !== null && <span data-testid="terminal-status">{status}</span>}
            {view.canReattach && (
              <button type="button" onClick={reattach}>
                Reattach
              </button>
            )}
            {view.canRetry && (
              <button type="button" onClick={retry}>
                Retry
              </button>
            )}
            {canClose && (
              <button
                type="button"
                disabled={closeDisabledTitle !== undefined}
                title={closeDisabledTitle}
                onClick={onClose}
              >
                Close
              </button>
            )}
          </div>
        )}
      </header>
      <div className="terminal-panel__body">
        <TerminalView
          ptySize={view.ptySize}
          cell={view.cell}
          letterboxed={view.letterboxed}
          dimmed={view.stale}
          overlay={overlayText(view)}
          hostRef={hostRef}
          onContainerSize={onContainerSize}
        />
      </div>
    </section>
  );
}
