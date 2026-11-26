import { type Dict, dictGet } from '../model/dict';
import type { HostId, TermId } from '../model/ids';
import type { LayoutNode, LayoutSnapshot, SplitDirection } from '../model/layout';
import type { OpenUrlPort, PanelPort } from '../ipc/port';
import type { TerminalPort } from '../ipc/terminalPort';
import { type SpaceKey } from '../model/space';
import { type SpacePanelsView, panelEntry, tabLabel } from '../model/panels';
import { HELD_NOTICE, HELD_TITLE, type HeldReason } from './heldLayout';
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
  onCloseTab?: (tile: string, panel: string) => void;
  onPinPanel?: (panel: string) => void;
  /** Panels with a close or pin in flight. */
  busyPanels?: ReadonlySet<string>;
  /** Why the tree is the last layout seen, if it is; its edit controls are disabled. */
  held?: HeldReason | null;
  actionError?: string | null;
}

type NodeProps = Omit<TileTreeProps, 'layout' | 'notice' | 'error' | 'actionError'> & {
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
    onCloseTab,
    onPinPanel,
    busyPanels,
    held = null,
  } = rest;
  const heldTitle = held === null ? undefined : HELD_TITLE[held];
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
          {node.panels.map((panel) => {
            const tabEntry = panelEntry(list, panel);
            const label = panelPort === undefined ? panel : tabLabel(list, panel);
            const busy = busyPanels?.has(panel) ?? false;
            const loading = tabEntry.kind === 'loading';
            const blocked = held !== null || busy || loading;
            const controlTitle = heldTitle ?? (busy ? 'Working…' : loading ? 'Loading…' : undefined);
            return (
              <span key={panel} role="presentation" className="tab-wrap">
                <button
                  type="button"
                  role="tab"
                  data-panel-id={panel}
                  data-panel-entry={panelPort === undefined ? undefined : tabEntry.kind}
                  aria-selected={panel === active}
                  onClick={() => onSelectTab(node.id, panel)}
                >
                  {label}
                </button>
                {panelPort !== undefined && onCloseTab !== undefined && (
                  <button
                    type="button"
                    className="tab-close"
                    aria-label={`Close panel ${label}`}
                    title={controlTitle ?? 'Close panel'}
                    disabled={blocked}
                    onClick={() => onCloseTab(node.id, panel)}
                  >
                    ×
                  </button>
                )}
                {panelPort !== undefined &&
                  onPinPanel !== undefined &&
                  tabEntry.kind === 'listed' &&
                  tabEntry.summary.lifetime === 'scratch' && (
                    <button
                      type="button"
                      className="tab-pin"
                      aria-label={`Pin panel ${label}`}
                      title={controlTitle ?? 'Keep it in the project'}
                      disabled={blocked}
                      onClick={() => onPinPanel(panel)}
                    >
                      Pin
                    </button>
                  )}
              </span>
            );
          })}
        </div>
        <button type="button" disabled={held !== null} title={heldTitle} onClick={() => onSplit(node.id, 'h')}>
          Split right
        </button>
        <button type="button" disabled={held !== null} title={heldTitle} onClick={() => onSplit(node.id, 'v')}>
          Split down
        </button>
        <button
          type="button"
          disabled={!closable || held !== null}
          title={heldTitle}
          onClick={() => onClose(node.id)}
        >
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

export function TileTree({ layout, notice, error, actionError = null, ...rest }: TileTreeProps) {
  const dropped = layout.dropped?.message ?? null;
  const held = rest.held ?? null;
  return (
    <main
      className="layout"
      aria-label="Layout"
      data-read-only={layout.readOnly ? 'true' : undefined}
      data-held={held ?? undefined}
    >
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
      {held !== null && (
        <p className="layout-held" role="status">
          {HELD_NOTICE[held]}
        </p>
      )}
      {actionError !== null && (
        <p className="panel-action-error" role="alert">
          {actionError}
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
