import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  type ClientState,
  defaultClientState,
  emptySpaceState,
  newestLastSpace,
  restoreSpaceState,
} from '../model/clientState';
import { type DecimalU64, type HostId, type TermId, decimalU64FromBigInt } from '../model/ids';
import { type SpaceKey, spaceKeyId } from '../model/space';
import type { ClientStatePort, HostsPort } from '../ipc/port';
import type { HostsSnapshot } from '../model/hosts';
import { CommandPalette } from './CommandPalette';
import { SpaceRail } from './SpaceRail';
import { type ActiveTerminals, TerminalNavigator } from './TerminalNavigator';
import { useHostsSnapshot } from './useHostsSnapshot';
import './shell.css';

interface AppProps {
  port: HostsPort;
  clientStatePort: ClientStatePort;
  now?: () => number;
}

type ClientStates = Readonly<Record<string, ClientState>>;

const NO_LAYOUT = {};

function errorText(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function findHostName(snapshot: HostsSnapshot, hostId: HostId): string {
  return snapshot.hosts.find((host) => host.hostId === hostId)?.name ?? hostId;
}

export function App({ port, clientStatePort, now = Date.now }: AppProps) {
  const { snapshot, error } = useHostsSnapshot(port);
  const [selected, setSelected] = useState<SpaceKey | null>(null);
  const [allSpaces, setAllSpaces] = useState(false);
  const [states, setStates] = useState<ClientStates>({});
  const [writeError, setWriteError] = useState<string | null>(null);
  const statesRef = useRef<ClientStates>({});
  const requested = useRef(new Set<HostId>());
  const restored = useRef(false);

  const updateStates = useCallback((change: (current: ClientStates) => ClientStates) => {
    statesRef.current = change(statesRef.current);
    setStates(statesRef.current);
  }, []);

  useEffect(() => {
    let live = true;
    for (const host of snapshot?.hosts ?? []) {
      const { hostId } = host;
      if (hostId === null || requested.current.has(hostId)) continue;
      requested.current.add(hostId);
      clientStatePort.load(hostId).then(
        (loaded) => {
          if (live) updateStates((current) => ({ ...current, [hostId]: loaded }));
        },
        (cause: unknown) => {
          if (live) {
            updateStates((current) => ({
              ...current,
              [hostId]: defaultClientState(errorText(cause)),
            }));
          }
        },
      );
    }
    return () => {
      live = false;
    };
  }, [snapshot, clientStatePort, updateStates]);

  useEffect(() => {
    if (restored.current || snapshot === null) return;
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
        if (activeTerminal !== null) active.set(spaceKeyId(space.key), activeTerminal);
      }
    }
    return active;
  }, [snapshot, states]);

  const persist = useCallback(
    (write: Promise<void>, hostId: HostId) => {
      write.catch((cause: unknown) => {
        const name = snapshot === null ? hostId : findHostName(snapshot, hostId);
        setWriteError(`Cannot save client state for ${name}: ${errorText(cause)}`);
      });
    },
    [snapshot],
  );

  const recordSelection = useCallback(
    (space: SpaceKey, terminalId: TermId | null) => {
      restored.current = true;
      setSelected(space);
      const { hostId, path } = space;
      const current = statesRef.current[hostId] ?? defaultClientState();
      const selectedAtMs: DecimalU64 = decimalU64FromBigInt(BigInt(Math.trunc(now())));
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
    [clientStatePort, now, persist, updateStates],
  );

  const selectSpace = useCallback(
    (space: SpaceKey) => recordSelection(space, null),
    [recordSelection],
  );
  const selectTerminal = useCallback(
    (space: SpaceKey, id: TermId) => recordSelection(space, id),
    [recordSelection],
  );
  const retry = useCallback((name: string) => void port.retryHost(name), [port]);

  if (error !== null) return <p role="alert">Cannot reach the Combine backend: {error}</p>;
  if (snapshot === null) return null;

  const stateErrors = snapshot.hosts.flatMap((host) => {
    const message = host.hostId === null ? null : (states[host.hostId]?.error ?? null);
    return message === null ? [] : [`Client state for ${host.name}: ${message}`];
  });
  if (writeError !== null) stateErrors.push(writeError);

  return (
    <div className="shell">
      <SpaceRail snapshot={snapshot} selected={selected} port={port} onSelect={selectSpace} />
      <TerminalNavigator
        snapshot={snapshot}
        selected={selected}
        active={activeTerminals}
        onSelectTerminal={selectTerminal}
        allSpaces={allSpaces}
        onToggleAllSpaces={() => setAllSpaces((on) => !on)}
      />
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
