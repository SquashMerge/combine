export type HeldReason = 'disconnected' | 'reconnecting' | 'failed';

/** Why the tree on screen is the last layout seen and not a live one. A failed load wins over the host's state. */
export function heldReason(hostConnected: boolean, error: string | null): HeldReason {
  if (error !== null) return 'failed';
  return hostConnected ? 'reconnecting' : 'disconnected';
}

export const HELD_TITLE: Record<HeldReason, string> = {
  disconnected: 'Host disconnected',
  reconnecting: 'Reconnecting',
  failed: 'Layout unavailable',
};

export const HELD_NOTICE: Record<HeldReason, string> = {
  disconnected:
    "Host disconnected. This is the last layout Combine saw; it can't be changed until the host reconnects.",
  reconnecting:
    "Reconnecting. This is the last layout Combine saw; it can't be changed until it loads again.",
  failed:
    "Couldn't load the layout. This is the last layout Combine saw, and it may be out of date; it can't be changed until it loads.",
};
