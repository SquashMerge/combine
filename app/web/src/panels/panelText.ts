import type { PanelView } from './attachmentMachine';

export function statusText(view: PanelView): string | null {
  const { status } = view;
  if (status === null) return null;
  switch (status.kind) {
    case 'error':
      return status.code;
    case 'streamFailed':
      return `Couldn't open the terminal stream: ${status.message}`;
    default:
      return status.kind;
  }
}

export function overlayText(view: PanelView): string | null {
  if (view.stale) return 'reconnecting';
  if (view.phase === 'waiting' && !view.connected) return 'not connected';
  if ((view.phase === 'attaching' || view.phase === 'resuming') && !view.hasInstance) {
    return 'attaching';
  }
  return null;
}

export function ownershipText(ownership: PanelView['ownership']): string {
  switch (ownership) {
    case 'self':
      return 'input: this panel';
    case 'other':
      return 'input: another panel';
    case 'none':
      return 'input: none';
    case 'unknown':
      return 'input: unknown';
  }
}

export function badges(view: PanelView): string[] {
  const result: string[] = [];
  if (view.fidelity === 'inexact') result.push('inexact');
  if (view.outputSkipped) result.push('output skipped');
  if (view.unorderedResizes) result.push('resizes may be out of order');
  return result;
}

export type GoneReason = 'unlisted' | 'backendGone' | 'noInstance';

export function goneText(reason: GoneReason, termId: string): string {
  switch (reason) {
    case 'unlisted':
      return `Terminal ${termId} is gone. The host no longer lists it.`;
    case 'backendGone':
      return `Terminal ${termId} is gone. It belonged to a terminal backend this host no longer runs.`;
    case 'noInstance':
      return `Terminal ${termId} can't be shown: the host doesn't say which terminal backend instance it runs.`;
  }
}
