import type { ClientSpaceState } from './clientState';
import type { HostsSnapshot, SpaceView, TerminalSummary } from './hosts';
import type { TermId } from './ids';
import { type SpaceKey, sameSpace } from './space';

/**
 * The terminal to open when a space is selected: the stored one while it is still listed (any
 * state), else the first listed terminal that has not exited, else none.
 */
export function pickAutoOpenTerminal(
  stored: ClientSpaceState | undefined,
  terminals: readonly TerminalSummary[],
): TermId | null {
  const active = stored?.activeTerminal ?? null;
  if (active !== null && terminals.some((t) => t.id === active)) return null;
  return terminals.find((t) => t.state !== 'exited')?.id ?? null;
}

export function findSpace(snapshot: HostsSnapshot, key: SpaceKey): SpaceView | undefined {
  for (const host of snapshot.hosts) {
    for (const space of host.spaces) {
      if (sameSpace(space.key, key)) return space;
    }
  }
  return undefined;
}
