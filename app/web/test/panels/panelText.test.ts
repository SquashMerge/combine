import { describe, expect, it } from 'vitest';
import { AttachmentMachine, type PanelView } from '../../src/panels/attachmentMachine';
import { parseTermId } from '../../src/model/ids';
import { badges, overlayText, ownershipText, statusText } from '../../src/panels/panelText';

const base: PanelView = new AttachmentMachine(parseTermId('t1')).view();

function view(patch: Partial<PanelView>): PanelView {
  return { ...base, ...patch };
}

describe('panelText', () => {
  it('statusText is the error code, exited, lost, closed or null', () => {
    expect(statusText(view({ status: null }))).toBeNull();
    expect(statusText(view({ status: { kind: 'exited' } }))).toBe('exited');
    expect(statusText(view({ status: { kind: 'lost' } }))).toBe('lost');
    expect(statusText(view({ status: { kind: 'closed' } }))).toBe('closed');
    expect(statusText(view({ status: { kind: 'error', code: 'send_failed' } }))).toBe('send_failed');
  });

  it('overlayText covers stale, disconnected waiting and attaching without an instance', () => {
    expect(overlayText(view({ stale: true, phase: 'attaching', hasInstance: true }))).toBe(
      'reconnecting',
    );
    expect(overlayText(view({ phase: 'waiting', connected: false }))).toBe('not connected');
    expect(overlayText(view({ phase: 'waiting', connected: true }))).toBeNull();
    expect(overlayText(view({ phase: 'attaching', hasInstance: false }))).toBe('attaching');
    expect(overlayText(view({ phase: 'resuming', hasInstance: false }))).toBe('attaching');
    expect(overlayText(view({ phase: 'attaching', hasInstance: true }))).toBeNull();
    expect(overlayText(view({ phase: 'live', hasInstance: true }))).toBeNull();
  });

  it('ownershipText names each ownership', () => {
    expect(ownershipText('self')).toBe('input: this panel');
    expect(ownershipText('other')).toBe('input: another panel');
    expect(ownershipText('none')).toBe('input: none');
    expect(ownershipText('unknown')).toBe('input: unknown');
  });

  it('badges lists inexact, output skipped and unordered resizes', () => {
    expect(
      badges(view({ fidelity: 'exact', outputSkipped: false, unorderedResizes: false })),
    ).toEqual([]);
    expect(
      badges(view({ fidelity: 'inexact', outputSkipped: true, unorderedResizes: true })),
    ).toEqual(['inexact', 'output skipped', 'resizes may be out of order']);
  });
});
