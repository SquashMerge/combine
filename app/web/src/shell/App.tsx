import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  type ClientSpaceState,
  type ClientState,
  defaultClientState,
  type LayoutTiles,
  emptySpaceState,
  newestLastSpace,
  restoreSpaceState,
  storedSpaceState,
  withoutActiveTabs,
} from '../model/clientState';
import { dictGet, dictWith, emptyDict } from '../model/dict';
import { type DecimalU64, type HostId, type TermId, decimalU64FromBigInt } from '../model/ids';
import { type SpaceKey, sameSpace, spaceKeyId } from '../model/space';
import type { ClientStatePort, HostsPort, LayoutPort, OpenUrlPort, PanelPort, SpawnPort } from '../ipc/port';
import type { TerminalPort } from '../ipc/terminalPort';
import type { XtermAdapterFactory } from '../renderers/terminal/adapter';
import type { HostsSnapshot, TerminalSummary } from '../model/hosts';
import {
  type LayoutNode,
  type LayoutSnapshot,
  firstTileId,
  layoutTiles,
  newlyActiveTiles,
  shownTab,
  tileHolding,
} from '../model/layout';
import { type PanelLifetime, panelEntry, tabLabel } from '../model/panels';
import {
  ACTIVE_TERMINAL_TAB,
  type TerminalTabsContext,
  isTerminalTab,
  terminalTabEntry,
  terminalTabId,
  terminalTabLabel,
} from '../model/terminalTabs';
import { AddTerminalMenu } from './AddTerminalMenu';
import { CommandPalette } from './CommandPalette';
import { ConfirmClosePanel } from './ConfirmClosePanel';
import { heldReason } from './heldLayout';
import type { PalettePanels } from './paletteEntries';
import { SpaceRail } from './SpaceRail';
import { TileTree } from './TileTree';
import { type ActiveTerminals, TerminalNavigator } from './TerminalNavigator';
import { useSpaceLayout } from './useSpaceLayout';
import { useAutoOpenTerminal } from './useAutoOpenTerminal';
import { useHostsSnapshot } from './useHostsSnapshot';
import './shell.css';

interface AppProps {
  port: HostsPort;
  clientStatePort: ClientStatePort;
  layoutPort: LayoutPort;
  spawnPort?: SpawnPort;
  panelPort?: PanelPort;
  openUrlPort?: OpenUrlPort;
  terminalPort: TerminalPort;
  createAdapter?: XtermAdapterFactory;
  newTileId?: () => string;
  now?: () => number;
}

type ClientStates = ReadonlyMap<HostId, ClientState>;

const NO_LAYOUT: LayoutTiles = new Map();
const NO_TABS = emptyDict<string>();

function errorText(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function findHostName(snapshot: HostsSnapshot | null, hostId: HostId): string {
  return (snapshot?.hosts ?? []).find((host) => host.hostId === hostId)?.name ?? hostId;
}

interface ConfirmClose {
  space: SpaceKey;
  panel: string;
  label: string;
  lifetime: PanelLifetime | null;
}

interface PendingSelection {
  space: SpaceKey;
  terminalId: TermId | null;
  selectedAtMs: DecimalU64;
}

export function App({
  port,
  clientStatePort,
  layoutPort,
  spawnPort,
  panelPort,
  openUrlPort,
  terminalPort,
  createAdapter,
  newTileId,
  now = Date.now,
}: AppProps) {
  const { snapshot, error } = useHostsSnapshot(port);
  const [selected, setSelected] = useState<SpaceKey | null>(null);
  const [allSpaces, setAllSpaces] = useState(false);
  const [states, setStates] = useState<ClientStates>(new Map());
  const [unsettled, setUnsettled] = useState<ReadonlyMap<string, TermId>>(new Map());
  const [writeErrors, setWriteErrors] = useState<ReadonlyMap<HostId, string>>(new Map());
  const [panelActionError, setPanelActionError] = useState<string | null>(null);
  const [busyActions, setBusyActions] = useState<ReadonlySet<string>>(new Set());
  const [confirmClose, setConfirmClose] = useState<ConfirmClose | null>(null);
  const statesRef = useRef<ClientStates>(new Map());
  const snapshotRef = useRef<HostsSnapshot | null>(null);
  const requested = useRef(new Set<HostId>());
  const pending = useRef(new Map<HostId, PendingSelection[]>());
  const mounted = useRef(false);
  const restored = useRef(false);
  const pendingFocus = useRef<{ space: string; panel: string } | null>(null);
  const previousRoot = useRef<{ space: string; root: LayoutNode } | null>(null);
  const selectedIdRef = useRef<string | null>(null);

  snapshotRef.current = snapshot;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const updateStates = useCallback((change: (current: ClientStates) => ClientStates) => {
    statesRef.current = change(statesRef.current);
    setStates(statesRef.current);
  }, []);

  const persist = useCallback((write: Promise<void>, hostId: HostId) => {
    write.then(
      () => {
        if (!mounted.current) return;
        setWriteErrors((current) => {
          if (!current.has(hostId)) return current;
          const next = new Map(current);
          next.delete(hostId);
          return next;
        });
      },
      (cause: unknown) => {
        if (!mounted.current) return;
        const name = findHostName(snapshotRef.current, hostId);
        setWriteErrors((current) =>
          new Map(current).set(hostId, `Cannot save client state for ${name}: ${errorText(cause)}`),
        );
      },
    );
  }, []);

  const applySelection = useCallback(
    ({ space, terminalId, selectedAtMs }: PendingSelection) => {
      const { hostId, path } = space;
      const current = statesRef.current.get(hostId) ?? defaultClientState();
      const spaceState = dictGet(current.spaces, path) ?? emptySpaceState(path);
      const nextSpace = terminalId === null ? spaceState : { ...spaceState, activeTerminal: terminalId };
      updateStates((all) =>
        new Map(all).set(hostId, {
          ...current,
          lastSpace: { spaceId: path, selectedAtMs },
          spaces: terminalId === null ? current.spaces : dictWith(current.spaces, path, nextSpace),
        }),
      );
      if (current.error !== null) return;
      if (terminalId !== null) persist(clientStatePort.setSpaceState(hostId, path, nextSpace), hostId);
      persist(clientStatePort.setLastSpace(hostId, path, selectedAtMs), hostId);
    },
    [clientStatePort, persist, updateStates],
  );

  const settle = useCallback(
    (hostId: HostId, loaded: ClientState) => {
      if (!mounted.current) return;
      updateStates((current) => new Map(current).set(hostId, loaded));
      const replay = pending.current.get(hostId) ?? [];
      pending.current.delete(hostId);
      if (replay.length === 0) return;
      for (const selection of replay) applySelection(selection);
      setUnsettled((current) => {
        const next = new Map(current);
        for (const { space } of replay) next.delete(spaceKeyId(space));
        return next;
      });
    },
    [applySelection, updateStates],
  );

  useEffect(() => {
    for (const host of snapshot?.hosts ?? []) {
      const { hostId } = host;
      if (hostId === null || requested.current.has(hostId)) continue;
      requested.current.add(hostId);
      clientStatePort.load(hostId).then(
        (loaded) => settle(hostId, loaded),
        (cause: unknown) => settle(hostId, defaultClientState(errorText(cause))),
      );
    }
  }, [snapshot, clientStatePort, settle]);

  useEffect(() => {
    if (restored.current || snapshot === null) return;
    const hostIds = snapshot.hosts.flatMap((host) => (host.hostId === null ? [] : [host.hostId]));
    if (!hostIds.every((hostId) => states.has(hostId))) return;
    const candidates = snapshot.hosts.flatMap((host) => {
      const hostId = host.hostId;
      const lastSpace = hostId === null ? null : (states.get(hostId)?.lastSpace ?? null);
      if (hostId === null || lastSpace === null) return [];
      const space = host.spaces.find((candidate) => candidate.key.path === lastSpace.spaceId);
      return space === undefined ? [] : [{ lastSpace, key: space.key }];
    });
    const newest = newestLastSpace(candidates);
    if (newest === null) return;
    restored.current = true;
    setSelected(newest.key);
  }, [snapshot, states]);

  const activeTerminals = useMemo<ActiveTerminals>(() => {
    const active = new Map<string, TermId>();
    for (const host of snapshot?.hosts ?? []) {
      if (host.hostId === null) continue;
      const stored = states.get(host.hostId);
      for (const space of host.spaces) {
        const terminalIds = new Set<string>(space.terminals.map((terminal) => terminal.id));
        const { activeTerminal } = restoreSpaceState(
          storedSpaceState(stored, space.key.path),
          NO_LAYOUT,
          terminalIds,
        );
        const id = spaceKeyId(space.key);
        const local = unsettled.get(id);
        if (local !== undefined) active.set(id, local);
        else if (activeTerminal !== null) active.set(id, activeTerminal);
      }
    }
    return active;
  }, [snapshot, states, unsettled]);

  const recordSelection = useCallback(
    (space: SpaceKey, terminalId: TermId | null) => {
      restored.current = true;
      setSelected(space);
      const selection: PendingSelection = {
        space,
        terminalId,
        selectedAtMs: decimalU64FromBigInt(BigInt(Math.trunc(now()))),
      };
      const { hostId } = space;
      if (statesRef.current.has(hostId)) {
        applySelection(selection);
        return;
      }
      pending.current.set(hostId, [...(pending.current.get(hostId) ?? []), selection]);
      if (terminalId !== null) {
        setUnsettled((current) => new Map(current).set(spaceKeyId(space), terminalId));
      }
    },
    [applySelection, now],
  );

  const selectSpace = useCallback(
    (space: SpaceKey) => recordSelection(space, null),
    [recordSelection],
  );
  const selectTerminal = useCallback(
    (space: SpaceKey, id: TermId) => recordSelection(space, id),
    [recordSelection],
  );
  const layout = useSpaceLayout(layoutPort, snapshot, selected, newTileId, panelPort);

  // useSpaceLayout drops its layout on any disconnect. Rendering the tree from the last layout of the
  // selected space keeps the terminal panel mounted through a host drop, so its controller reconnects
  // and resumes instead of being stopped and re-attached. The held copy is read-only until the watch
  // replies again.
  const lastLayout = useRef<{ space: string; snapshot: LayoutSnapshot } | null>(null);
  const selectedId = selected === null ? null : spaceKeyId(selected);
  if (layout.snapshot !== null && selectedId !== null) {
    lastLayout.current = { space: selectedId, snapshot: layout.snapshot };
  }
  const shownLayout = useMemo<LayoutSnapshot | null>(() => {
    if (layout.snapshot !== null) return layout.snapshot;
    const held = lastLayout.current;
    return held !== null && held.space === selectedId ? { ...held.snapshot, readOnly: true } : null;
  }, [layout.snapshot, selectedId]);

  const restoredTiles = useMemo(() => {
    if (shownLayout === null || selected === null) return null;
    const space = snapshot?.hosts
      .find((host) => host.hostId === selected.hostId)
      ?.spaces.find((candidate) => candidate.key.path === selected.path);
    const terminalIds = new Set<string>(space?.terminals.map((terminal) => terminal.id) ?? []);
    const { root } = shownLayout;
    const result = restoreSpaceState(
      storedSpaceState(states.get(selected.hostId), selected.path),
      layoutTiles(root),
      terminalIds,
    );
    return { ...result, focusedTile: result.focusedTile ?? firstTileId(root) };
  }, [shownLayout, selected, snapshot, states]);

  const terminalTabs = useMemo<TerminalTabsContext | null>(() => {
    if (selected === null) return null;
    const hostView = snapshot?.hosts.find((candidate) => candidate.hostId === selected.hostId);
    if (hostView === undefined) return null;
    const terminals = new Map<TermId, TerminalSummary>();
    for (const hostSpace of hostView.spaces) {
      for (const terminal of hostSpace.terminals) terminals.set(terminal.id, terminal);
    }
    const spaceTerminals =
      hostView.spaces.find((candidate) => candidate.key.path === selected.path)?.terminals ?? [];
    return {
      hostId: selected.hostId,
      hostName: hostView.name,
      backend: hostView.backend ?? null,
      terminals,
      spaceTerminals,
      activeTerminal: activeTerminals.get(spaceKeyId(selected)) ?? null,
    };
  }, [activeTerminals, selected, snapshot]);

  const changeSpaceState = useCallback(
    (space: SpaceKey, change: (state: ClientSpaceState) => ClientSpaceState) => {
      const { hostId, path } = space;
      const current = statesRef.current.get(hostId);
      if (current === undefined) return;
      const base = dictGet(current.spaces, path) ?? emptySpaceState(path);
      const next = change(base);
      if (next === base) return;
      updateStates((all) =>
        new Map(all).set(hostId, { ...current, spaces: dictWith(current.spaces, path, next) }),
      );
      if (current.error === null) persist(clientStatePort.setSpaceState(hostId, path, next), hostId);
    },
    [clientStatePort, persist, updateStates],
  );

  const autoOpen = useAutoOpenTerminal(snapshot, selected, states, changeSpaceState);

  const focusTile = useCallback(
    (tile: string) => {
      if (selected === null) return;
      changeSpaceState(selected, (state) =>
        state.focusedTile === tile ? state : { ...state, focusedTile: tile },
      );
    },
    [changeSpaceState, selected],
  );

  const selectTab = useCallback(
    (tile: string, panel: string) => {
      if (selected === null) return;
      changeSpaceState(selected, (state) =>
        dictGet(state.activeTabs, tile) === panel
          ? state
          : { ...state, activeTabs: dictWith(state.activeTabs, tile, panel) },
      );
    },
    [changeSpaceState, selected],
  );

  const hostConnected =
    selected !== null &&
    snapshot?.hosts.some(
      (host) => host.hostId === selected.hostId && host.connection.kind === 'connected',
    ) === true;
  const held =
    shownLayout !== null && layout.snapshot === null ? heldReason(hostConnected, layout.error) : null;
  const panelList = layout.panels?.list ?? null;

  useEffect(() => {
    selectedIdRef.current = selectedId;
    previousRoot.current = null;
    setPanelActionError(null);
    setConfirmClose(null);
  }, [selectedId]);

  useEffect(() => {
    if (held !== null) setConfirmClose(null);
  }, [held]);

  // A panel the daemon put in front of a tile replaces whatever tab the user had chosen there, and a
  // panel placed from the palette takes focus once a tile holds it.
  useEffect(() => {
    const next = layout.snapshot;
    if (next === null || selected === null || selectedId === null) return;
    const before = previousRoot.current;
    previousRoot.current = { space: selectedId, root: next.root };
    const named = before !== null && before.space === selectedId ? newlyActiveTiles(before.root, next.root) : [];
    const wanted = pendingFocus.current;
    const landed =
      wanted !== null && wanted.space === selectedId ? tileHolding(next.root, wanted.panel) : null;
    if (named.length === 0 && landed === null) return;
    const change = (state: ClientSpaceState): ClientSpaceState => {
      const cleared = withoutActiveTabs(state, named);
      return landed === null || cleared.focusedTile === landed ? cleared : { ...cleared, focusedTile: landed };
    };
    const current = storedSpaceState(statesRef.current.get(selected.hostId), selected.path);
    const base = current ?? emptySpaceState(selected.path);
    if (current !== undefined || landed !== null) {
      if (change(base) !== base) changeSpaceState(selected, change);
    }
    if (landed !== null) pendingFocus.current = null;
  }, [layout.snapshot, selected, selectedId, changeSpaceState]);

  const sendPanelAction = useCallback(
    (kind: 'close' | 'pin', space: SpaceKey, panel: string, label: string) => {
      if (panelPort === undefined) return;
      const spaceId = spaceKeyId(space);
      const key = `${spaceId}:${panel}`;
      setPanelActionError(null);
      setBusyActions((current) => new Set(current).add(key));
      const request =
        kind === 'close'
          ? panelPort.closePanel(space.hostId, space.path, panel)
          : panelPort.pinPanel(space.hostId, space.path, panel);
      request
        .catch((cause: unknown) => {
          if (!mounted.current || selectedIdRef.current !== spaceId) return;
          const verb = kind === 'close' ? 'close' : 'pin';
          setPanelActionError(`Couldn't ${verb} ${label}: ${errorText(cause)}`);
        })
        .finally(() => {
          if (!mounted.current) return;
          setBusyActions((current) => {
            const next = new Set(current);
            next.delete(key);
            return next;
          });
        });
    },
    [panelPort],
  );

  const closePanel = useCallback(
    (space: SpaceKey, panel: string) => {
      if (isTerminalTab(panel)) {
        // A terminal tab is only ever taken out of the layout; the terminal itself is never closed.
        if (terminalTabs !== null && terminalTabEntry(panel, terminalTabs)?.kind === 'pending') return;
        layout.removePanel(panel);
        return;
      }
      const entry = panelEntry(panelList, panel);
      const label = tabLabel(panelList, panel);
      if (entry.kind === 'missing' || entry.kind === 'skipped') {
        layout.removePanel(panel);
        return;
      }
      if (entry.kind === 'loading') return;
      const { lifetime } = entry.summary;
      if (lifetime === 'persistent') sendPanelAction('close', space, panel, label);
      else setConfirmClose({ space, panel, label, lifetime });
    },
    [layout, panelList, sendPanelAction, terminalTabs],
  );

  const pinPanel = useCallback(
    (space: SpaceKey, panel: string) =>
      sendPanelAction('pin', space, panel, tabLabel(panelList, panel)),
    [panelList, sendPanelAction],
  );

  const showPanel = useCallback(
    (space: SpaceKey, panel: string, tile: string | null) => {
      if (selectedId === null || spaceKeyId(space) !== selectedId) {
        selectSpace(space);
        return;
      }
      if (tile === null) {
        pendingFocus.current = { space: selectedId, panel };
        layout.addPanel(panel);
        return;
      }
      changeSpaceState(space, (state) => ({
        ...state,
        focusedTile: tile,
        activeTabs: dictWith(state.activeTabs, tile, panel),
      }));
    },
    [changeSpaceState, layout, selectSpace, selectedId],
  );

  const openTerminal = useCallback(
    (space: SpaceKey, id: TermId) => {
      recordSelection(space, id);
      const live = layout.snapshot;
      if (selected === null || !sameSpace(space, selected) || live === null || live.readOnly) return;
      if (held !== null || restoredTiles === null) return;
      const instance = terminalTabs?.backend?.instance ?? null;
      const tab = instance === null ? null : terminalTabId(instance, id);
      if (tab === null) return;
      const focusedId = restoredTiles.focusedTile;
      const focusedData = layoutTiles(live.root).get(focusedId);
      if (focusedData === undefined) return;
      const shown = shownTab({ id: focusedId, ...focusedData }, restoredTiles.activeTabs);
      if (shown === ACTIVE_TERMINAL_TAB) return;
      const holder = tileHolding(live.root, tab);
      if (holder !== null) {
        changeSpaceState(space, (state) => ({
          ...state,
          focusedTile: holder,
          activeTabs: dictWith(state.activeTabs, holder, tab),
        }));
        return;
      }
      pendingFocus.current = { space: spaceKeyId(space), panel: tab };
      if (focusedData.panels.length === 0) layout.addPanelTo(tab, focusedId);
      else layout.openBeside(focusedId, tab);
    },
    [changeSpaceState, held, layout, recordSelection, restoredTiles, selected, terminalTabs],
  );

  const repinTerminal = useCallback(
    (previous: string, next: string, tile: string) => layout.replacePanel(previous, next, tile),
    [layout],
  );

  const selectActiveTerminal = useCallback(
    (id: string) => {
      if (selected !== null) selectTerminal(selected, id as TermId);
    },
    [selectTerminal, selected],
  );

  const showActiveTerminalHere = useCallback(
    (space: SpaceKey, tile: string) => {
      if (shownLayout === null || selectedId === null || spaceKeyId(space) !== selectedId) return;
      const holder = tileHolding(shownLayout.root, ACTIVE_TERMINAL_TAB);
      if (holder === tile) {
        // Already in this tile, perhaps as a hidden tab: only show it.
      } else if (holder === null) layout.addPanelTo(ACTIVE_TERMINAL_TAB, tile);
      else layout.movePanelTo(ACTIVE_TERMINAL_TAB, tile, layoutTiles(shownLayout.root).get(tile)?.panels.length ?? 0);
      changeSpaceState(space, (state) => ({
        ...state,
        focusedTile: tile,
        activeTabs: dictWith(state.activeTabs, tile, ACTIVE_TERMINAL_TAB),
      }));
    },
    [changeSpaceState, layout, selectedId, shownLayout],
  );

  const busyPanels = useMemo(() => {
    const prefix = `${selectedId}:`;
    return new Set([...busyActions].filter((key) => key.startsWith(prefix)).map((key) => key.slice(prefix.length)));
  }, [busyActions, selectedId]);

  const palettePanels = useMemo<PalettePanels | null>(() => {
    if (selected === null || shownLayout === null || !hostConnected) return null;
    if (layout.panels === null || !layout.panels.connected || panelList === null) return null;
    const tiles = new Map<string, string>();
    const all = layoutTiles(shownLayout.root);
    for (const [id, tile] of all) {
      for (const panel of tile.panels) tiles.set(panel, id);
    }
    const focusedId = restoredTiles?.focusedTile ?? null;
    const focusedTileData = focusedId === null ? undefined : all.get(focusedId);
    const panel =
      focusedId === null || focusedTileData === undefined
        ? null
        : shownTab({ id: focusedId, ...focusedTileData }, restoredTiles?.activeTabs ?? NO_TABS);
    const terminalEntry =
      panel === null || terminalTabs === null ? null : terminalTabEntry(panel, terminalTabs);
    return {
      space: selected,
      hostName: findHostName(snapshot, selected.hostId),
      panels: panelList.panels,
      tiles,
      focused:
        focusedId === null || panel === null
          ? null
          : {
              tile: focusedId,
              panel,
              ...(terminalEntry === null ? {} : { label: terminalTabLabel(panel, terminalEntry) }),
            },
      focusedTile: focusedTileData === undefined ? null : focusedId,
      activeTerminalTile: tileHolding(shownLayout.root, ACTIVE_TERMINAL_TAB),
      editable: held === null && layout.snapshot !== null && !layout.snapshot.readOnly,
    };
  }, [selected, shownLayout, hostConnected, layout.panels, layout.snapshot, panelList, restoredTiles, snapshot, held, terminalTabs]);

  const retry = useCallback((name: string) => void port.retryHost(name), [port]);

  if (error !== null) return <p role="alert">Cannot reach the Combine backend: {error}</p>;
  if (snapshot === null) return null;

  const stateErrors = snapshot.hosts.flatMap((host) => {
    const message = host.hostId === null ? null : (states.get(host.hostId)?.error ?? null);
    return message === null ? [] : [`Client state for ${host.name}: ${message}`];
  });
  stateErrors.push(...writeErrors.values());

  return (
    <div className="shell">
      <SpaceRail
        snapshot={snapshot}
        selected={selected}
        port={port}
        readOnlySpaces={layout.readOnlySpaces}
        onSelect={selectSpace}
      />
      <TerminalNavigator
        snapshot={snapshot}
        selected={selected}
        active={activeTerminals}
        onSelectTerminal={openTerminal}
        allSpaces={allSpaces}
        onToggleAllSpaces={() => setAllSpaces((on) => !on)}
        actions={
          spawnPort === undefined ? undefined : (
            <AddTerminalMenu
              port={spawnPort}
              snapshot={snapshot}
              selected={selected}
              onSpawned={autoOpen.activateWhenListed}
            />
          )
        }
      />
      {shownLayout === null && layout.error !== null && (
        <p className="layout-error" role="alert">
          {layout.error}
        </p>
      )}
      {shownLayout !== null && restoredTiles !== null && (
        <TileTree
          layout={shownLayout}
          notice={layout.notice}
          error={layout.error}
          focusedTile={restoredTiles.focusedTile}
          activeTabs={restoredTiles.activeTabs}
          onFocus={focusTile}
          onSelectTab={selectTab}
          onSplit={layout.split}
          onClose={layout.close}
          terminalPort={terminalPort}
          terminalTabs={terminalTabs}
          onSelectActiveTerminal={selectActiveTerminal}
          onRepinTerminal={repinTerminal}
          space={selected}
          panels={layout.panels}
          held={held}
          actionError={panelActionError}
          busyPanels={busyPanels}
          onCloseTab={(_tile, panel) => selected !== null && closePanel(selected, panel)}
          onPinPanel={(panel) => selected !== null && pinPanel(selected, panel)}
          {...(panelPort === undefined ? {} : { panelPort })}
          {...(openUrlPort === undefined ? {} : { openUrlPort })}
          {...(createAdapter === undefined ? {} : { createAdapter })}
        />
      )}
      <CommandPalette
        snapshot={snapshot}
        onSelectSpace={selectSpace}
        onSelectTerminal={openTerminal}
        onRetryHost={retry}
        panels={palettePanels}
        onShowPanel={showPanel}
        onClosePanel={closePanel}
        onPinPanel={pinPanel}
        onShowActiveTerminal={showActiveTerminalHere}
      />
      {confirmClose !== null && (
        <ConfirmClosePanel
          label={confirmClose.label}
          lifetime={confirmClose.lifetime}
          onCancel={() => setConfirmClose(null)}
          onConfirm={() => {
            sendPanelAction('close', confirmClose.space, confirmClose.panel, confirmClose.label);
            setConfirmClose(null);
          }}
        />
      )}
      {stateErrors.length > 0 && (
        <div className="client-state-error" role="alert">
          {stateErrors.map((message) => (
            <p key={message}>{message}</p>
          ))}
        </div>
      )}
    </div>
  );
}
