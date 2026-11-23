import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { LayoutPort } from '../ipc/port';
import type { HostsSnapshot } from '../model/hosts';
import { type DecimalU64, type HostId, compareDecimalU64 } from '../model/ids';
import type { LayoutEvent, LayoutSnapshot, SplitDirection } from '../model/layout';
import { LayoutQueue } from '../model/layoutQueue';
import { type SpaceKey, spaceKeyId } from '../model/space';

const NOT_CONNECTED = 'not connected';

export interface SpaceLayout {
  snapshot: LayoutSnapshot | null;
  notice: string | null;
  error: string | null;
  readOnlySpaces: ReadonlySet<string>;
  split: (tile: string, dir: SplitDirection) => void;
  close: (tile: string) => void;
}

interface Held {
  hostId: HostId;
  snapshot: LayoutSnapshot;
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
 * host's layout events to the snapshot and to that space's mutation queue.
 */
export function useSpaceLayout(
  port: LayoutPort,
  hosts: HostsSnapshot | null,
  selected: SpaceKey | null,
  newTileId?: () => string,
): SpaceLayout {
  const [held, setHeld] = useState<Held | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [readOnly, setReadOnly] = useState<ReadonlySet<string>>(new Set());
  const watched = useRef<SpaceKey | null>(null);
  const watchRequest = useRef<DecimalU64 | null>(null);
  const queue = useRef<LayoutQueue | null>(null);

  const watchHost =
    selected !== null && isConnected(hosts, selected.hostId) ? selected.hostId : null;
  const watchPath = watchHost === null ? null : (selected?.path ?? null);

  useEffect(() => {
    const prev = watched.current;
    const next = watchHost !== null && watchPath !== null ? { hostId: watchHost, path: watchPath } : null;
    if (prev !== null && (next === null || next.hostId !== prev.hostId)) {
      port.unwatchLayout(prev.hostId).catch((cause: unknown) => {
        if (errorText(cause) !== NOT_CONNECTED) setError(errorText(cause));
      });
    }
    if (next !== null && (prev === null || prev.hostId !== next.hostId || prev.path !== next.path)) {
      queue.current?.onDisconnected();
      queue.current = new LayoutQueue({
        send: (base, ops) => port.mutateLayout(next.hostId, next.path, base, ops),
        onNotice: setNotice,
        ...(newTileId === undefined ? {} : { newTileId }),
      });
      watchRequest.current = null;
      setHeld(null);
      setNotice(null);
      setError(null);
      port.watchLayout(next.hostId, next.path).then(
        (request) => {
          watchRequest.current = request;
        },
        (cause: unknown) => setError(errorText(cause)),
      );
    }
    if (next === null) {
      queue.current?.onDisconnected();
      queue.current = null;
      setHeld(null);
    }
    watched.current = next;
  }, [port, watchHost, watchPath, newTileId]);

  useEffect(
    () => () => {
      const prev = watched.current;
      watched.current = null;
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
          if (event.request === watchRequest.current) setError(event.message);
          return;
        case 'disconnected':
          queue.current?.onDisconnected();
          setHeld(null);
          setReadOnly((current) => {
            const id = spaceKeyId(target);
            if (!current.has(id)) return current;
            const next = new Set(current);
            next.delete(id);
            return next;
          });
      }
    };

    port.onLayoutEvent(handle).then(
      (stop) => {
        if (active) unsubscribe = stop;
        else stop();
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

  const split = useCallback(
    (tile: string, dir: SplitDirection) => queue.current?.split(tile, dir),
    [],
  );
  const close = useCallback((tile: string) => queue.current?.close(tile), []);

  const snapshot =
    held !== null && selected !== null && held.hostId === selected.hostId && held.snapshot.space === selected.path
      ? held.snapshot
      : null;

  return useMemo(
    () => ({ snapshot, notice, error, readOnlySpaces: readOnly, split, close }),
    [snapshot, notice, error, readOnly, split, close],
  );
}
