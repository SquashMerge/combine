import { type Dict, dictGet } from '../model/dict';
import type { HostId, TermId } from '../model/ids';
import type { LayoutNode, LayoutSnapshot, SplitDirection } from '../model/layout';
import type { OpenUrlPort, PanelPort } from '../ipc/port';
import type { TerminalPort } from '../ipc/terminalPort';
import { type SpaceKey } from '../model/space';
import { type SpacePanelsView, panelEntry, tabLabel } from '../model/panels';
import { TerminalPanel } from '../panels/TerminalPanel';
import { ViewPanel } from '../panels/ViewPanel';
import type { XtermAdapterFactory } from '../renderers/terminal/adapter';

export interface TerminalMount {
  hostId: HostId;
  termId: TermId;
  label: { name: string; host: string; state: string };
}

interface TileTreeProps {
  layout: LayoutSnapshot;
  notice: string | null;
  error: string | null;
  focusedTile: string | null;
  activeTabs: Readonly<Dict<string>>;
  onFocus: (tile: string) => void;
  onSelectTab: (tile: string, panel: string) => void;
  onSplit: (tile: string, dir: SplitDirection) => void;
  onClose: (tile: string) => void;
  terminalPort: TerminalPort;
  terminal: TerminalMount | null;
  createAdapter?: XtermAdapterFactory;
  space?: SpaceKey | null;
  panels?: SpacePanelsView | null;
  panelPort?: PanelPort;
  openUrlPort?: OpenUrlPort;
}

type NodeProps = Omit<TileTreeProps, 'layout' | 'notice' | 'error'> & {
  node: LayoutNode;
  closable: boolean;
};

function Node({ node, closable, ...rest }: NodeProps) {
  if (node.type === 'split') {
    return (
      <div className="split" data-dir={node.dir}>
        <div className="split-pane" style={{ flex: `${node.ratio} 1 0px` }}>
          <Node node={node.a} closable={closable} {...rest} />
        </div>
        <div className="split-pane" style={{ flex: `${1 - node.ratio} 1 0px` }}>
          <Node node={node.b} closable={closable} {...rest} />
        </div>
      </div>
    );
  }
  const {
    focusedTile,
    activeTabs,
    onFocus,
    onSelectTab,
    onSplit,
    onClose,
    terminalPort,
    terminal,
    createAdapter,
    space = null,
    panels = null,
    panelPort,
    openUrlPort,
  } = rest;
  const active = dictGet(activeTabs, node.id) ?? node.active ?? node.panels[0] ?? null;
  const focused = focusedTile === node.id;
  const list = panels?.list ?? null;
  const entry = active === null || panels === null ? null : panelEntry(list, active);
  // A listed or skipped panel takes the focused tile; a dangling or loading one leaves the terminal.
  const viewWins = entry?.kind === 'listed' || entry?.kind === 'skipped';
  // The panel fills the whole body: a placeholder beside it would overflow the container the PTY is sized from.
  const hasTerminal = focused && terminal !== null && !viewWins;
  return (
    <div
      className="tile"
      data-tile-id={node.id}
      aria-current={focused ? 'true' : undefined}
      onClick={() => onFocus(node.id)}
    >
      <div className="tile-bar">
        <div role="tablist" aria-label={`Panels of ${node.id}`}>
          {node.panels.map((panel) => (
            <button
              key={panel}
              type="button"
              role="tab"
              data-panel-id={panel}
              data-panel-entry={panelPort === undefined ? undefined : panelEntry(list, panel).kind}
              aria-selected={panel === active}
              onClick={() => onSelectTab(node.id, panel)}
            >
              {panelPort === undefined ? panel : tabLabel(list, panel)}
            </button>
          ))}
        </div>
        <button type="button" onClick={() => onSplit(node.id, 'h')}>
          Split right
        </button>
        <button type="button" onClick={() => onSplit(node.id, 'v')}>
          Split down
        </button>
        <button type="button" disabled={!closable} onClick={() => onClose(node.id)}>
          Close tile
        </button>
      </div>
      <div className="tile-body" role="tabpanel">
        {!hasTerminal &&
          (active === null ? (
            'No panels'
          ) : panelPort === undefined || space === null ? (
            active
          ) : (
            <ViewPanel
              key={`${space.hostId}/${space.path}/${active}`}
              hostId={space.hostId}
              space={space.path}
              panelId={active}
              focused={focused}
              entry={panelEntry(list, active)}
              session={panels?.session ?? 0}
              connected={panels?.connected ?? false}
              port={panelPort}
              {...(openUrlPort === undefined ? {} : { openUrlPort })}
            />
          ))}
        {hasTerminal && (
          // Never render under React.StrictMode: a panel's controller is created once and stop() is permanent.
          <TerminalPanel
            key={`${terminal.hostId}/${terminal.termId}`}
            port={terminalPort}
            hostId={terminal.hostId}
            termId={terminal.termId}
            visible
            label={terminal.label}
            {...(createAdapter === undefined ? {} : { createAdapter })}
          />
        )}
      </div>
    </div>
  );
}

export function TileTree({ layout, notice, error, ...rest }: TileTreeProps) {
  const dropped = layout.dropped?.message ?? null;
  return (
    <main className="layout" aria-label="Layout" data-read-only={layout.readOnly ? 'true' : undefined}>
      {layout.error !== null && (
        <p className="layout-error" role="alert">
          {layout.error}
        </p>
      )}
      {error !== null && <p role="alert">{error}</p>}
      {rest.panels?.list?.error != null && (
        <p className="panels-error" role="alert">
          {rest.panels.list.error}
        </p>
      )}
      {dropped !== null && (
        <p className="layout-notice" role="status">
          {dropped}
        </p>
      )}
      {notice !== null && (
        <p className="layout-notice" role="status">
          {notice}
        </p>
      )}
      <Node node={layout.root} closable={layout.root.type === 'split'} {...rest} />
    </main>
  );
}
