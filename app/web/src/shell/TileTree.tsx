import { useMemo } from 'react';
import type { Dict } from '../model/dict';
import type { TermId } from '../model/ids';
import { type LayoutNode, type LayoutSnapshot, type SplitDirection, layoutTiles, shownTab } from '../model/layout';
import type { OpenUrlPort, PanelPort } from '../ipc/port';
import type { TerminalPort } from '../ipc/terminalPort';
import { type SpaceKey } from '../model/space';
import { type SpacePanelsView, panelEntry, tabLabel } from '../model/panels';
import {
  type TerminalTabEntry,
  type TerminalTabsContext,
  isTerminalTab,
  terminalTabEntry,
  terminalTabId,
  terminalTabKind,
  terminalTabLabel,
} from '../model/terminalTabs';
import { HELD_NOTICE, HELD_TITLE, type HeldReason } from './heldLayout';
import { GoneTerminal } from '../panels/GoneTerminal';
import { goneMarked } from '../panels/panelText';
import { TerminalPanel } from '../panels/TerminalPanel';
import { ViewPanel } from '../panels/ViewPanel';
import type { XtermAdapterFactory } from '../renderers/terminal/adapter';

const NO_ACTIVE_TERMINAL = 'No active terminal. Pick one in the navigator.';
const PENDING_TERMINALS = "Waiting for the host's terminal list…";
const ALREADY_OPEN_TITLE = 'Already open in this layout';
const UNPINNABLE_TITLE = "This terminal can't be pinned: its ID is invalid or too long";


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
  /** What the terminal tabs of this space resolve against; null when the host isn't known. */
  terminalTabs?: TerminalTabsContext | null;
  /** Picks the terminal the follow-active tab shows. */
  onSelectActiveTerminal?: (terminal: string) => void;
  /** Re-pins a pinned terminal tab of a tile to another terminal. */
  onRepinTerminal?: (previous: string, terminal: string, tile: string) => void;
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
  /** Every tab the layout holds. */
  openTabs: ReadonlySet<string>;
};

interface TerminalTabBodyProps {
  tabId: string;
  tile: string;
  entry: TerminalTabEntry;
  ctx: TerminalTabsContext;
  focused: boolean;
  held: HeldReason | null;
  openTabs: ReadonlySet<string>;
  terminalPort: TerminalPort;
  onCloseTab?: ((tile: string, panel: string) => void) | undefined;
  onSelectActiveTerminal?: ((terminal: string) => void) | undefined;
  onRepinTerminal?: ((previous: string, terminal: string, tile: string) => void) | undefined;
  createAdapter?: XtermAdapterFactory;
}

function TerminalTabBody({
  tabId,
  tile,
  entry,
  ctx,
  focused,
  held,
  openTabs,
  terminalPort,
  onCloseTab,
  onSelectActiveTerminal,
  onRepinTerminal,
  createAdapter,
}: TerminalTabBodyProps) {
  const heldTitle = held === null ? undefined : HELD_TITLE[held];
  switch (entry.kind) {
    case 'unknown':
      return (
        <p className="view-panel-status" role="status">
          Panel <code>{tabId}</code> is missing
        </p>
      );
    case 'pending':
      return (
        <p className="view-panel-status" role="status">
          {PENDING_TERMINALS}
        </p>
      );
    case 'gone':
      return (
        <GoneTerminal
          message={goneMarked(entry.why, entry.terminal)}
          onClose={() => onCloseTab?.(tile, tabId)}
          {...(heldTitle === undefined ? {} : { disabledTitle: heldTitle })}
        />
      );
    case 'follow-active':
    case 'pinned':
      break;
  }
  const { terminal } = entry;
  if (terminal === null) {
    return (
      <p className="view-panel-status" role="status">
        {NO_ACTIVE_TERMINAL}
      </p>
    );
  }
  const pinned = entry.kind === 'pinned';
  const backendInstance = ctx.backend?.instance ?? null;
  // A pin is host-wide, so its terminal may sit in another space: it is still the current
  // option, so the select always has an option matching its value.
  const candidates = ctx.spaceTerminals.some((candidate) => candidate.id === terminal.id)
    ? ctx.spaceTerminals
    : [terminal, ...ctx.spaceTerminals];
  const options = candidates.map((candidate) => {
    const label = `${candidate.name} (${candidate.state})`;
    if (!pinned) return { id: candidate.id, label, disabled: false };
    const candidateTab = backendInstance === null ? null : terminalTabId(backendInstance, candidate.id);
    const open = candidateTab !== null && candidate.id !== terminal.id && openTabs.has(candidateTab);
    // A held layout disables every option, and its reason comes first.
    const title = heldTitle ?? (candidateTab === null ? UNPINNABLE_TITLE : open ? ALREADY_OPEN_TITLE : undefined);
    return {
      id: candidate.id,
      label: open ? `${candidate.name} (already open)` : candidateTab === null ? `${candidate.name} (can't be pinned)` : label,
      disabled: held !== null || open || candidateTab === null,
      ...(title === undefined ? {} : { title }),
    };
  });
  const onSwitch = (id: string): void => {
    if (!pinned) {
      onSelectActiveTerminal?.(id);
      return;
    }
    const next = backendInstance === null ? null : terminalTabId(backendInstance, id as TermId);
    if (next !== null) onRepinTerminal?.(tabId, next, tile);
  };
  // A pinned panel names its backend instance, so it never attaches to another core's terminal
  // that reuses the ID; the follow-active tab names none.
  const panelInstance = pinned ? backendInstance : null;
  // Never render under React.StrictMode: a panel's controller is created once and stop() is permanent.
  return (
    <TerminalPanel
      key={`${ctx.hostId}/${panelInstance ?? ''}/${terminal.id}`}
      port={terminalPort}
      hostId={ctx.hostId}
      termId={terminal.id}
      backendInstance={panelInstance}
      visible
      label={{ name: terminal.name, host: ctx.hostName, state: terminal.state, backend: ctx.backend?.id ?? null }}
      focused={focused}
      switcher={{ options, onSwitch }}
      {...(pinned
        ? {
            onClose: () => onCloseTab?.(tile, tabId),
            ...(heldTitle === undefined ? {} : { closeDisabledTitle: heldTitle }),
          }
        : {})}
      {...(createAdapter === undefined ? {} : { createAdapter })}
    />
  );
}

function Node({ node, closable, openTabs, ...rest }: NodeProps) {
  if (node.type === 'split') {
    return (
      <div className="split" data-dir={node.dir}>
        <div className="split-pane" style={{ flex: `${node.ratio} 1 0px` }}>
          <Node node={node.a} closable={closable} openTabs={openTabs} {...rest} />
        </div>
        <div className="split-pane" style={{ flex: `${1 - node.ratio} 1 0px` }}>
          <Node node={node.b} closable={closable} openTabs={openTabs} {...rest} />
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
    terminalTabs = null,
    onSelectActiveTerminal,
    onRepinTerminal,
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
  const active = shownTab(node, activeTabs);
  const focused = focusedTile === node.id;
  const list = panels?.list ?? null;
  // A tab ID that starts with `.` is a terminal tab, whatever the panel list says.
  const terminalEntry = (id: string): TerminalTabEntry | null =>
    terminalTabs === null
      ? isTerminalTab(id)
        ? { kind: 'unknown' }
        : null
      : terminalTabEntry(id, terminalTabs);
  const activeEntry = active === null ? null : terminalEntry(active);
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
            const terminalTab = terminalEntry(panel);
            if (terminalTab !== null) {
              const label = terminalTabLabel(panel, terminalTab);
              const pending = terminalTab.kind === 'pending';
              return (
                <span key={panel} role="presentation" className="tab-wrap">
                  <button
                    type="button"
                    role="tab"
                    data-panel-id={panel}
                    data-panel-entry={terminalTabKind(terminalTab)}
                    aria-selected={panel === active}
                    onClick={() => onSelectTab(node.id, panel)}
                  >
                    {label}
                  </button>
                  {onCloseTab !== undefined && (
                    <button
                      type="button"
                      className="tab-close"
                      aria-label={`Close panel ${label}`}
                      title={heldTitle ?? (pending ? 'Loading…' : 'Close panel')}
                      disabled={held !== null || pending}
                      onClick={() => onCloseTab(node.id, panel)}
                    >
                      ×
                    </button>
                  )}
                </span>
              );
            }
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
        {active === null ? (
          'No panels'
        ) : activeEntry !== null && terminalTabs !== null ? (
          <TerminalTabBody
            tabId={active}
            tile={node.id}
            entry={activeEntry}
            ctx={terminalTabs}
            focused={focused}
            held={held}
            openTabs={openTabs}
            terminalPort={terminalPort}
            onCloseTab={onCloseTab}
            onSelectActiveTerminal={onSelectActiveTerminal}
            onRepinTerminal={onRepinTerminal}
            {...(createAdapter === undefined ? {} : { createAdapter })}
          />
        ) : activeEntry !== null ? (
          <p className="view-panel-status" role="status">
            Panel <code>{active}</code> is missing
          </p>
        ) : panelPort === undefined || space === null ? (
          <code>{active}</code>
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
        )}
      </div>
    </div>
  );
}

export function TileTree({ layout, notice, error, actionError = null, ...rest }: TileTreeProps) {
  const dropped = layout.dropped?.message ?? null;
  const held = rest.held ?? null;
  const openTabs = useMemo(
    () => new Set([...layoutTiles(layout.root).values()].flatMap((tile) => tile.panels)),
    [layout.root],
  );
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
      <Node
        node={layout.root}
        closable={layout.root.type === 'split'}
        openTabs={openTabs}
        {...rest}
      />
    </main>
  );
}
