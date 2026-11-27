import { describe, expect, it } from 'vitest';
import { parseTermId } from '../../src/model/ids';
import { AttachmentMachine, type MachineEffect } from '../../src/panels/attachmentMachine';
import {
  attachedEvent,
  connectionEvent,
  cursor,
  gapEvent,
  ownershipEvent,
  startedEvent,
} from '../support/memoryTerminalPort';

const TERM = parseTermId('term-1');
const FIT = { cols: 100, rows: 20 };

interface Rig {
  machine: AttachmentMachine;
  /** Feeds an input and returns the effects it produced. */
  feed(input: Parameters<AttachmentMachine['handle']>[0]): MachineEffect[];
  ownership(generation: number, owner: number | null): MachineEffect[];
}

/** A visible, connected machine with a container that fits 100x20, live as attachment 7. */
function liveMachine({ focused = false, container = true } = {}): Rig {
  const machine = new AttachmentMachine(TERM);
  const feed: Rig['feed'] = (input) => machine.handle(input);
  feed({ kind: 'focused', focused });
  if (container) feed({ kind: 'containerSize', width: 800, height: 340 });
  feed({ kind: 'show' });
  feed({ kind: 'event', event: connectionEvent(true) });
  feed({ kind: 'event', event: attachedEvent(1, 7) });
  feed({ kind: 'event', event: startedEvent(cursor(7, 1, 0)) });
  feed({ kind: 'applied', cursor: cursor(7, 1, 0) });
  return {
    machine,
    feed,
    ownership: (generation, owner) =>
      feed({ kind: 'event', event: ownershipEvent(7, generation, owner) }),
  };
}

function claimOps(effects: MachineEffect[]): MachineEffect[] {
  return effects.filter(
    (effect) => effect.kind === 'resize' || effect.kind === 'requestOwnership',
  );
}

describe('attachmentMachine claim', () => {
  it('claims a terminal with no owner after storing its fit', () => {
    const t = liveMachine({ focused: true });
    expect(claimOps(t.ownership(4, null))).toEqual([
      { kind: 'resize', attachment: '7', size: FIT, ownershipGeneration: '4' },
      { kind: 'requestOwnership', attachment: '7' },
    ]);
  });

  it('claims without a resize when there is no fit yet', () => {
    const t = liveMachine({ focused: true, container: false });
    expect(claimOps(t.ownership(4, null))).toEqual([
      { kind: 'requestOwnership', attachment: '7' },
    ]);
  });

  it('claims once per generation', () => {
    const t = liveMachine({ focused: true });
    expect(claimOps(t.ownership(4, null))).toHaveLength(2);
    expect(claimOps(t.ownership(4, null))).toEqual([]);
    expect(claimOps(t.feed({ kind: 'focused', focused: true }))).toEqual([]);
    expect(claimOps(t.feed({ kind: 'show' }))).toEqual([]);
  });

  it('unfocused never claims', () => {
    const t = liveMachine();
    expect(claimOps(t.ownership(4, null))).toEqual([]);
    expect(claimOps(t.ownership(5, null))).toEqual([]);
  });

  it('claims on becoming focused', () => {
    const t = liveMachine();
    t.ownership(4, null);
    expect(claimOps(t.feed({ kind: 'focused', focused: true }))).toEqual([
      { kind: 'resize', attachment: '7', size: FIT, ownershipGeneration: '4' },
      { kind: 'requestOwnership', attachment: '7' },
    ]);
  });

  it('claims when the owner becomes null', () => {
    const t = liveMachine({ focused: true });
    expect(claimOps(t.ownership(4, 8))).toEqual([]);
    expect(claimOps(t.ownership(5, null))).toEqual([
      { kind: 'resize', attachment: '7', size: FIT, ownershipGeneration: '5' },
      { kind: 'requestOwnership', attachment: '7' },
    ]);
  });

  it('does not claim when another attachment owns the terminal', () => {
    const t = liveMachine({ focused: true });
    expect(claimOps(t.ownership(4, 8))).toEqual([]);
    expect(claimOps(t.feed({ kind: 'focused', focused: true }))).toEqual([]);
  });

  it('no claim before the first ownership', () => {
    const t = liveMachine({ focused: true });
    expect(t.machine.view().ownership).toBe('unknown');
    expect(claimOps(t.feed({ kind: 'containerSize', width: 900, height: 340 }))).toEqual([]);
  });

  it('claims again after a new attachment has the same generation', () => {
    const t = liveMachine({ focused: true });
    t.ownership(4, null);
    t.feed({ kind: 'event', event: gapEvent(7, null) });
    t.feed({ kind: 'event', event: attachedEvent(2, 8) });
    t.feed({ kind: 'event', event: startedEvent(cursor(8, 1, 0)) });
    t.feed({ kind: 'applied', cursor: cursor(8, 1, 0) });
    const effects = t.feed({ kind: 'event', event: ownershipEvent(8, 4, null) });
    expect(claimOps(effects)).toEqual([
      { kind: 'resize', attachment: '8', size: FIT, ownershipGeneration: '4' },
      { kind: 'requestOwnership', attachment: '8' },
    ]);
  });

  it('take input stores the fit first', () => {
    const t = liveMachine();
    t.ownership(4, 8);
    expect(claimOps(t.feed({ kind: 'takeInput' }))).toEqual([
      { kind: 'resize', attachment: '7', size: FIT, ownershipGeneration: '4' },
      { kind: 'requestOwnership', attachment: '7' },
    ]);
  });

  it('a claimed fit is not sent again by the resize timer at the same generation', () => {
    const t = liveMachine({ focused: true });
    t.ownership(4, null);
    t.ownership(4, 7);
    expect(claimOps(t.feed({ kind: 'timer', timer: 'resize' }))).toEqual([]);
  });
});

describe('attachmentMachine stream failure', () => {
  it('shows streamFailed and offers a retry until the retry starts', () => {
    const machine = new AttachmentMachine(TERM);
    machine.handle({ kind: 'show' });
    machine.handle({ kind: 'streamFailed', message: 'no link' });
    expect(machine.view()).toMatchObject({
      status: { kind: 'streamFailed', message: 'no link' },
      canRetry: true,
    });
    machine.handle({ kind: 'hide' });
    machine.handle({ kind: 'show' });
    expect(machine.view().canRetry).toBe(true);
    machine.handle({ kind: 'retryStream' });
    expect(machine.view()).toMatchObject({ status: null, canRetry: false });
  });
});
