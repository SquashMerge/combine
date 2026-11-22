import { useEffect, useState } from 'react';
import type { HostsSnapshot } from '../model/hosts';
import { compareDecimalU64 } from '../model/ids';
import type { HostsPort } from '../ipc/port';

export interface HostsSnapshotState {
  snapshot: HostsSnapshot | null;
  error: string | null;
}

export function useHostsSnapshot(port: HostsPort): HostsSnapshotState {
  const [snapshot, setSnapshot] = useState<HostsSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | null = null;

    const apply = (next: HostsSnapshot): void => {
      if (!active) return;
      setSnapshot((current) =>
        current === null || compareDecimalU64(next.revision, current.revision) > 0 ? next : current,
      );
    };

    const start = async (): Promise<void> => {
      const stop = await port.onSnapshot(apply);
      if (active) unsubscribe = stop;
      else stop();
      apply(await port.getSnapshot());
    };

    start().catch((cause: unknown) => {
      if (active) setError(cause instanceof Error ? cause.message : String(cause));
    });

    return () => {
      active = false;
      unsubscribe?.();
    };
  }, [port]);

  return { snapshot, error };
}
