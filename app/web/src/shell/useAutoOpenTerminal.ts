import { useCallback, useEffect, useState } from 'react';
import { pickAutoOpenTerminal, findSpace } from '../model/autoOpen';
import type { ClientSpaceState, ClientState } from '../model/clientState';
import { dictGet } from '../model/dict';
import type { HostId, TermId } from '../model/ids';
import type { HostsSnapshot } from '../model/hosts';
import { type SpaceKey, sameSpace } from '../model/space';

const ACTIVATION_TIMEOUT_MS = 10_000;

interface PendingActivation {
  space: SpaceKey;
  terminal: TermId;
}

type ChangeSpaceState = (
  space: SpaceKey,
  change: (state: ClientSpaceState) => ClientSpaceState,
) => void;

function activate(terminal: TermId) {
  return (state: ClientSpaceState): ClientSpaceState =>
    state.activeTerminal === terminal ? state : { ...state, activeTerminal: terminal };
}

/**
 * Makes a terminal the space's active one without touching `lastSpace`: the first running terminal
 * when a space is selected and nothing valid is stored, and a just-spawned terminal once the
 * snapshot lists it. Never spawns.
 */
export function useAutoOpenTerminal(
  snapshot: HostsSnapshot | null,
  selected: SpaceKey | null,
  states: ReadonlyMap<HostId, ClientState>,
  changeSpaceState: ChangeSpaceState,
): { activateWhenListed: (space: SpaceKey, terminal: TermId) => void } {
  const [pending, setPending] = useState<PendingActivation | null>(null);

  const activateWhenListed = useCallback(
    (space: SpaceKey, terminal: TermId) => setPending({ space, terminal }),
    [],
  );

  useEffect(() => {
    if (pending === null) return;
    const timer = setTimeout(() => setPending(null), ACTIVATION_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [pending]);

  useEffect(() => {
    if (pending === null || snapshot === null || !states.has(pending.space.hostId)) return;
    const space = findSpace(snapshot, pending.space);
    if (space === undefined || !space.terminals.some((t) => t.id === pending.terminal)) return;
    changeSpaceState(pending.space, activate(pending.terminal));
    setPending(null);
  }, [pending, snapshot, states, changeSpaceState]);

  useEffect(() => {
    if (selected === null || snapshot === null) return;
    if (pending !== null && sameSpace(pending.space, selected)) return;
    const stored = states.get(selected.hostId);
    if (stored === undefined) return;
    const space = findSpace(snapshot, selected);
    if (space === undefined) return;
    const pick = pickAutoOpenTerminal(dictGet(stored.spaces, selected.path), space.terminals);
    if (pick === null) return;
    changeSpaceState(selected, activate(pick));
  }, [pending, snapshot, selected, states, changeSpaceState]);

  return { activateWhenListed };
}
