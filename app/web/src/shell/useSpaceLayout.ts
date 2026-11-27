import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { LayoutPort, PanelPort } from '../ipc/port';
import type { HostsSnapshot } from '../model/hosts';
import { type DecimalU64, type HostId, compareDecimalU64 } from '../model/ids';
import type { LayoutEvent, LayoutSnapshot, SplitDirection } from '../model/layout';
import { LayoutQueue } from '../model/layoutQueue';
import { type PanelList, type PanelsEvent, type SpacePanelsView, isLoadingList } from '../model/panels';
import { type SpaceKey, spaceKeyId } from '../model/space';

const NOT_CONNECTED = 'not connected';

export interface SpaceLayout {
  snapshot: LayoutSnapshot | null;
  notice: string | null;
  error: string | null;
  /** The selected space's panels; null without a panel port. */
  panels: SpacePanelsView | null;
  readOnlySpaces: ReadonlySet<string>;
  split: (tile: string, dir: SplitDirection) => void;
  close: (tile: string) => void;
  /** Puts the panel in a tile of the daemon's choosing. */
  addPanel: (panel: string) => void;
  /** Takes the panel out of every tile. */
  removePanel: (panel: string) => void;
  /** Puts the panel in the named tile. */
  addPanelTo: (panel: string, tile: string) => void;
  /** Splits the tile to the right and puts the panel in the new tile, as one batch. */
  openBeside: (tile: string, panel: string) => void;
  /** Adds `next` to the tile, then removes `previous`. */
  replacePanel: (previous: string, next: string, tile: string) => void;
  /** Moves the panel to the tile at the index. */
  movePanelTo: (panel: string, toTile: string, index: number) => void;
}

interface Held {
  hostId: HostId;
  snapshot: LayoutSnapshot;
}

interface PanelsHeld {
  hostId: HostId;
  space: string;
  list: PanelList | null;
  connected: boolean;
}

function errorText(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function isConnected(hosts: HostsSnapshot | null, hostId: HostId): boolean {
  return (hosts?.hosts ?? []).some(
    (host) => host.hostId === hostId && host.connection.kind === 'connected',
  );
}

/**
 * Keeps one layout watch on the selected space while its host is connected, and routes the
 * host's layout events to the snapshot and to that space's mutation queue. The watch is sent only
 * after the layout-event listener is registered, as `useHostsSnapshot` subscribes before it fetches.
 */
export function useSpaceLayout(
  port: LayoutPort,
  hosts: HostsSnapshot | null,
  selected: SpaceKey | null,
  newTileId?: () => string,
  panelPort?: PanelPort,
): SpaceLayout {
  const [held, setHeld] = useState<Held | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [readOnly, setReadOnly] = useState<ReadonlySet<string>>(new Set());
  // The port whose layout-event listener is registered. No watch is sent before then: a quick
  // reply to it would arrive with no listener and be lost.
  const [listening, setListening] = useState<LayoutPort | null>(null);
  const [panelsListening, setPanelsListening] = useState<PanelPort | null>(null);
  const [panelsHeld, setPanelsHeld] = useState<PanelsHeld | null>(null);
  // Goes up each time the watched host's connection ends; the view panel hosts report again on it.
  const [panelsSession, setPanelsSession] = useState(0);
  const panelsSessionRef = useRef(0);
  const watched = useRef<SpaceKey | null>(null);
  const watchRequest = useRef<DecimalU64 | null>(null);
  // Bumped whenever the watch is replaced, dropped or unmounted, so a late watch reply from an
  // older watch is ignored.
  const watchGeneration = useRef(0);
  const queue = useRef<LayoutQueue | null>(null);

  const ready = listening === port && (panelPort === undefined || panelsListening === panelPort);
  const watchHost =
    ready && selected !== null && isConnected(hosts, selected.hostId) ? selected.hostId : null;
  const watchPath = watchHost === null ? null : (selected?.path ?? null);

  useEffect(() => {
    const prev = watched.current;
    const next = watchHost !== null && watchPath !== null ? { hostId: watchHost, path: watchPath } : null;
    if (prev !== null && (next === null || next.hostId !== prev.hostId)) {
      port.unwatchLayout(prev.hostId).catch((cause: unknown) => {
        if (errorText(cause) !== NOT_CONNECTED) setError(errorText(cause));
      });
    }
    // Set before the watch goes out, so a reply that comes back at once is routed to it.
    watched.current = next;
    if (next !== null && (prev === null || prev.hostId !== next.hostId || prev.path !== next.path)) {
      queue.current?.onDisconnected();
      setPanelsHeld((current) =>
        current !== null && current.hostId === next.hostId && current.space === next.path
          ? current.connected
            ? { ...current, connected: false }
            : current
          : { hostId: next.hostId, space: next.path, list: null, connected: false },
      );
      queue.current = new LayoutQueue({
        send: (base, ops) => port.mutateLayout(next.hostId, next.path, base, ops),
        onNotice: setNotice,
        ...(newTileId === undefined ? {} : { newTileId }),
      });
      watchRequest.current = null;
      setHeld(null);
      setNotice(null);
      setError(null);
      watchGeneration.current += 1;
      const generation = watchGeneration.current;
      port.watchLayout(next.hostId, next.path).then(
        (request) => {
          if (watchGeneration.current === generation) watchRequest.current = request;
        },
        (cause: unknown) => {
          if (watchGeneration.current === generation) setError(errorText(cause));
        },
      );
    }
    if (next === null) {
      watchGeneration.current += 1;
      queue.current?.onDisconnected();
      queue.current = null;
      setHeld(null);
      setPanelsHeld((current) =>
        current === null || !current.connected ? current : { ...current, connected: false },
      );
    }
  }, [port, watchHost, watchPath, newTileId]);

  useEffect(
    () => () => {
      const prev = watched.current;
      watched.current = null;
      watchGeneration.current += 1;
      if (prev !== null) port.unwatchLayout(prev.hostId).catch(() => undefined);
    },
    [port],
  );

  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | null = null;

    const handle = (event: LayoutEvent): void => {
      const target = watched.current;
      if (!active || target === null || event.hostId !== target.hostId) return;
      switch (event.kind) {
        case 'snapshot': {
          const { snapshot } = event;
          if (snapshot.space !== target.path) return;
          setHeld((current) =>
            current === null ||
            compareDecimalU64(snapshot.layoutRevision, current.snapshot.layoutRevision) >= 0
              ? { hostId: target.hostId, snapshot }
              : current,
          );
          const id = spaceKeyId(target);
          setReadOnly((current) => {
            if (current.has(id) === snapshot.readOnly) return current;
            const next = new Set(current);
            if (snapshot.readOnly) next.add(id);
            else next.delete(id);
            return next;
          });
          queue.current?.onSnapshot(event.request, snapshot);
          return;
        }
        case 'error':
          queue.current?.onError(event.request, event.code, event.message);
          if (watchRequest.current === null || event.request === watchRequest.current) {
            setError(event.message);
          }
          return;
        case 'disconnected':
          queue.current?.onDisconnected();
          setHeld(null);
      }
    };

    port.onLayoutEvent(handle).then(
      (stop) => {
        if (!active) {
          stop();
          return;
        }
        unsubscribe = stop;
        setListening(port);
      },
      (cause: unknown) => {
        if (active) setError(errorText(cause));
      },
    );
    return () => {
      active = false;
      unsubscribe?.();
    };
  }, [port]);

  useEffect(() => {
    if (panelPort === undefined) return undefined;
    let active = true;
    let unsubscribe: (() => void) | null = null;

    const handle = (event: PanelsEvent): void => {
      const target = watched.current;
      if (!active || target === null || event.hostId !== target.hostId) return;
      if (event.kind === 'disconnected') {
        panelsSessionRef.current += 1;
        setPanelsSession(panelsSessionRef.current);
        setPanelsHeld((current) =>
          current !== null && current.hostId === target.hostId && current.connected
            ? { ...current, connected: false }
            : current,
        );
        return;
      }
      if (event.list.space !== target.path) return;
      const list = isLoadingList(event.list) ? null : event.list;
      setPanelsHeld({ hostId: target.hostId, space: target.path, list, connected: true });
    };

    panelPort.onPanelsEvent(handle).then(
      (stop) => {
        if (!active) {
          stop();
          return;
        }
        unsubscribe = stop;
        setPanelsListening(panelPort);
      },
      (cause: unknown) => {
        if (active) setError(errorText(cause));
      },
    );
    return () => {
      active = false;
      unsubscribe?.();
    };
  }, [panelPort]);

  const split = useCallback(
    (tile: string, dir: SplitDirection) => queue.current?.split(tile, dir),
    [],
  );
  const close = useCallback((tile: string) => queue.current?.close(tile), []);
  const addPanel = useCallback((panel: string) => queue.current?.addPanel(panel), []);
  const removePanel = useCallback((panel: string) => queue.current?.removePanel(panel), []);
  const addPanelTo = useCallback(
    (panel: string, tile: string) => queue.current?.addPanelTo(panel, tile),
    [],
  );
  const openBeside = useCallback(
    (tile: string, panel: string) => queue.current?.openBeside(tile, panel),
    [],
  );
  const replacePanel = useCallback(
    (previous: string, next: string, tile: string) => queue.current?.replacePanel(previous, next, tile),
    [],
  );
  const movePanelTo = useCallback(
    (panel: string, toTile: string, index: number) => queue.current?.movePanelTo(panel, toTile, index),
    [],
  );

  const snapshot =
    held !== null && selected !== null && held.hostId === selected.hostId && held.snapshot.space === selected.path
      ? held.snapshot
      : null;

  const panels = useMemo<SpacePanelsView | null>(() => {
    if (panelPort === undefined || selected === null) return null;
    const held =
      panelsHeld !== null && panelsHeld.hostId === selected.hostId && panelsHeld.space === selected.path
        ? panelsHeld
        : null;
    return { session: panelsSession, connected: held?.connected ?? false, list: held?.list ?? null };
  }, [panelPort, selected, panelsHeld, panelsSession]);

  return useMemo(
    () => ({
      snapshot,
      notice,
      error,
      panels,
      readOnlySpaces: readOnly,
      split,
      close,
      addPanel,
      removePanel,
      addPanelTo,
      openBeside,
      replacePanel,
      movePanelTo,
    }),
    [
      snapshot,
      notice,
      error,
      panels,
      readOnly,
      split,
      close,
      addPanel,
      removePanel,
      addPanelTo,
      openBeside,
      replacePanel,
      movePanelTo,
    ],
  );
}
