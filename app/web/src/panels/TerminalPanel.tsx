import type { HostId, TermId } from '../model/ids';
import type { TerminalPort } from '../ipc/terminalPort';
import type { XtermAdapterFactory } from '../renderers/terminal/adapter';
import { TerminalView } from '../renderers/terminal/TerminalView';
import { createXtermAdapter } from '../renderers/terminal/xtermAdapter';
import { badges, overlayText, ownershipText, statusText } from './panelText';
import { useTerminalPanel } from './useTerminalPanel';
import './terminalPanel.css';

export interface TerminalPanelProps {
  port: TerminalPort;
  hostId: HostId;
  termId: TermId;
  visible: boolean;
  label: { name: string; host: string; state: string };
  createAdapter?: XtermAdapterFactory;
}

/** `hostId` and `termId` are fixed for the panel's lifetime: key it by `${hostId}/${termId}`. */
export function TerminalPanel({
  port,
  hostId,
  termId,
  visible,
  label,
  createAdapter = createXtermAdapter,
}: TerminalPanelProps) {
  const { view, hostRef, onContainerSize, takeInput, reattach } = useTerminalPanel({
    port,
    hostId,
    termId,
    visible,
    createAdapter,
  });
  const status = statusText(view);

  return (
    <section className="terminal-panel" hidden={!visible} data-testid="terminal-panel">
      <header className="terminal-panel__header">
        <div className="terminal-panel__title">
          <span className="terminal-panel__name">{label.name}</span>
          <span className="terminal-panel__state">{label.state}</span>
          <span className="terminal-panel__host">{label.host}</span>
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
        {(status !== null || view.canReattach) && (
          <div className="terminal-panel__status">
            {status !== null && <span data-testid="terminal-status">{status}</span>}
            {view.canReattach && (
              <button type="button" onClick={reattach}>
                Reattach
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
