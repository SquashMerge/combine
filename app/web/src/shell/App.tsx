import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  type ClientSpaceState,
  type ClientState,
  defaultClientState,
  emptySpaceState,
  newestLastSpace,
  restoreSpaceState,
} from '../model/clientState';
import { type DecimalU64, type HostId, type TermId, decimalU64FromBigInt } from '../model/ids';
import { type SpaceKey, spaceKeyId } from '../model/space';
import type { ClientStatePort, HostsPort, LayoutPort } from '../ipc/port';
import type { HostsSnapshot } from '../model/hosts';
import { firstTileId, layoutTiles } from '../model/layout';
import { CommandPalette } from './CommandPalette';
import { SpaceRail } from './SpaceRail';
import { TileTree } from './TileTree';
import { type ActiveTerminals, TerminalNavigator } from './TerminalNavigator';
import { useSpaceLayout } from './useSpaceLayout';
import { useHostsSnapshot } from './useHostsSnapshot';
import './shell.css';

interface AppProps {
  port: HostsPort;
  clientStatePort: ClientStatePort;
  layoutPort: LayoutPort;
  newTileId?: () => string;
  now?: () => number;
}

type ClientStates = Readonly<Record<string, ClientState>>;

const NO_LAYOUT = {};

function errorText(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function findHostName(snapshot: HostsSnapshot | null, hostId: HostId): string {
  return (snapshot?.hosts ?? []).find((host) => host.hostId === hostId)?.name ?? hostId;
}

interface PendingSelection {
  space: SpaceKey;
  terminalId: TermId | null;
  selectedAtMs: DecimalU64;
}

export function App({ port, clientStatePort, layoutPort, newTileId, now = Date.now }: AppProps) {
  const { snapshot, error } = useHostsSnapshot(port);
  const [selected, setSelected] = useState<SpaceKey | null>(null);
  const [allSpaces, setAllSpaces] = useState(false);
  const [states, setStates] = useState<ClientStates>({});
  const [unsettled, setUnsettled] = useState<ReadonlyMap<string, TermId>>(new Map());
  const [writeErrors, setWriteErrors] = useState<Readonly<Record<string, string>>>({});
  const statesRef = useRef<ClientStates>({});
  const snapshotRef = useRef<HostsSnapshot | null>(null);
  const requested = useRef(new Set<HostId>());
  const pending = useRef(new Map<HostId, PendingSelection[]>());
  const mounted = useRef(false);
  const restored = useRef(false);

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
        setWriteErrors(({ [hostId]: _cleared, ...rest }) => rest);
      },
      (cause: unknown) => {
        if (!mounted.current) return;
        const name = findHostName(snapshotRef.current, hostId);
        setWriteErrors((current) => ({
          ...current,
          [hostId]: `Cannot save client state for ${name}: ${errorText(cause)}`,
        }));
      },
    );
  }, []);

  const applySelection = useCallback(
    ({ space, terminalId, selectedAtMs }: PendingSelection) => {
      const { hostId, path } = space;
      const current = statesRef.current[hostId] ?? defaultClientState();
      const spaceState = current.spaces[path] ?? emptySpaceState(path);
      const nextSpace = terminalId === null ? spaceState : { ...spaceState, activeTerminal: terminalId };
      updateStates((all) => ({
        ...all,
        [hostId]: {
          ...current,
          lastSpace: { spaceId: path, selectedAtMs },
          spaces: terminalId === null ? current.spaces : { ...current.spaces, [path]: nextSpace },
        },
      }));
      if (current.error !== null) return;
      if (terminalId !== null) persist(clientStatePort.setSpaceState(hostId, path, nextSpace), hostId);
      persist(clientStatePort.setLastSpace(hostId, path, selectedAtMs), hostId);
    },
    [clientStatePort, persist, updateStates],
  );

  const settle = useCallback(
    (hostId: HostId, loaded: ClientState) => {
      if (!mounted.current) return;
      updateStates((current) => ({ ...current, [hostId]: loaded }));
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
    if (!hostIds.every((hostId) => states[hostId] !== undefined)) return;
    const candidates = snapshot.hosts.flatMap((host) => {
      const hostId = host.hostId;
      const lastSpace = hostId === null ? null : (states[hostId]?.lastSpace ?? null);
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
      const stored = states[host.hostId]?.spaces;
      for (const space of host.spaces) {
        const terminalIds = new Set<string>(space.terminals.map((terminal) => terminal.id));
        const { activeTerminal } = restoreSpaceState(stored?.[space.key.path], NO_LAYOUT, terminalIds);
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
      if (statesRef.current[hostId] !== undefined) {
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
  const layout = useSpaceLayout(layoutPort, snapshot, selected, newTileId);

  const restoredTiles = useMemo(() => {
    if (layout.snapshot === null || selected === null) return null;
    const space = snapshot?.hosts
      .find((host) => host.hostId === selected.hostId)
      ?.spaces.find((candidate) => candidate.key.path === selected.path);
    const terminalIds = new Set<string>(space?.terminals.map((terminal) => terminal.id) ?? []);
    const { root } = layout.snapshot;
    const result = restoreSpaceState(
      states[selected.hostId]?.spaces[selected.path],
      layoutTiles(root),
      terminalIds,
    );
    return { ...result, focusedTile: result.focusedTile ?? firstTileId(root) };
  }, [layout.snapshot, selected, snapshot, states]);

  const changeSpaceState = useCallback(
    (space: SpaceKey, change: (state: ClientSpaceState) => ClientSpaceState) => {
      const { hostId, path } = space;
      const current = statesRef.current[hostId];
      if (current === undefined) return;
      const next = change(current.spaces[path] ?? emptySpaceState(path));
      updateStates((all) => ({
        ...all,
        [hostId]: { ...current, spaces: { ...current.spaces, [path]: next } },
      }));
      if (current.error === null) persist(clientStatePort.setSpaceState(hostId, path, next), hostId);
    },
    [clientStatePort, persist, updateStates],
  );

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
      changeSpaceState(selected, (state) => ({
        ...state,
        activeTabs: { ...state.activeTabs, [tile]: panel },
      }));
    },
    [changeSpaceState, selected],
  );

  const retry = useCallback((name: string) => void port.retryHost(name), [port]);

  if (error !== null) return <p role="alert">Cannot reach the Combine backend: {error}</p>;
  if (snapshot === null) return null;

  const stateErrors = snapshot.hosts.flatMap((host) => {
    const message = host.hostId === null ? null : (states[host.hostId]?.error ?? null);
    return message === null ? [] : [`Client state for ${host.name}: ${message}`];
  });
  stateErrors.push(...Object.values(writeErrors));

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
        onSelectTerminal={selectTerminal}
        allSpaces={allSpaces}
        onToggleAllSpaces={() => setAllSpaces((on) => !on)}
      />
      {layout.snapshot === null && layout.error !== null && (
        <p className="layout-error" role="alert">
          {layout.error}
        </p>
      )}
      {layout.snapshot !== null && restoredTiles !== null && (
        <TileTree
          layout={layout.snapshot}
          notice={layout.notice}
          error={layout.error}
          focusedTile={restoredTiles.focusedTile}
          activeTabs={restoredTiles.activeTabs}
          onFocus={focusTile}
          onSelectTab={selectTab}
          onSplit={layout.split}
          onClose={layout.close}
        />
      )}
      <CommandPalette
        snapshot={snapshot}
        onSelectSpace={selectSpace}
        onSelectTerminal={selectTerminal}
        onRetryHost={retry}
      />
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
