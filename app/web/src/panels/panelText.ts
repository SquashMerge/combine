import type { PanelView } from './attachmentMachine';

export function statusText(view: PanelView): string | null {
  const { status } = view;
  if (status === null) return null;
  return status.kind === 'error' ? status.code : status.kind;
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
