import { describe, expect, it } from 'vitest';
import { parseTermId } from '../../src/model/ids';
import { AttachmentMachine, type MachineEffect } from '../../src/panels/attachmentMachine';
import {
  attachedEvent,
  connectionEvent,
  cursor,
  errorEvent,
  gapEvent,
  ownershipEvent,
  resumedEvent,
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
      { kind: 'requestOwnership', attachment: '7', ifVacant: '4' },
    ]);
  });

  it('claims without a resize when there is no fit yet', () => {
    const t = liveMachine({ focused: true, container: false });
    expect(claimOps(t.ownership(4, null))).toEqual([
      { kind: 'requestOwnership', attachment: '7', ifVacant: '4' },
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
      { kind: 'requestOwnership', attachment: '7', ifVacant: '4' },
    ]);
  });

  it('claims when the owner becomes null', () => {
    const t = liveMachine({ focused: true });
    expect(claimOps(t.ownership(4, 8))).toEqual([]);
    expect(claimOps(t.ownership(5, null))).toEqual([
      { kind: 'resize', attachment: '7', size: FIT, ownershipGeneration: '5' },
      { kind: 'requestOwnership', attachment: '7', ifVacant: '5' },
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
      { kind: 'requestOwnership', attachment: '8', ifVacant: '4' },
    ]);
  });

  it('claims again after a resume at the same generation', () => {
    const t = liveMachine({ focused: true });
    expect(claimOps(t.ownership(4, null))).toHaveLength(2);
    t.feed({ kind: 'event', event: connectionEvent(false) });
    const reconnect = t.feed({ kind: 'event', event: connectionEvent(true) });
    expect(reconnect.filter((effect) => effect.kind === 'resume')).toHaveLength(1);
    t.feed({ kind: 'event', event: resumedEvent(2, 7, null) });
    expect(claimOps(t.ownership(4, null))).toEqual([
      { kind: 'resize', attachment: '7', size: FIT, ownershipGeneration: '4' },
      { kind: 'requestOwnership', attachment: '7', ifVacant: '4' },
    ]);
    expect(claimOps(t.ownership(4, null))).toEqual([]);
  });

  it('take input stores the fit first', () => {
    const t = liveMachine();
    t.ownership(4, 8);
    expect(claimOps(t.feed({ kind: 'takeInput' }))).toEqual([
      { kind: 'resize', attachment: '7', size: FIT, ownershipGeneration: '4' },
      { kind: 'requestOwnership', attachment: '7', ifVacant: null },
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

const opsOf = (effects: MachineEffect[]) =>
  effects.filter((effect) => effect.kind === 'attach' || effect.kind === 'resume');

/** A visible pinned machine for backend instance `core-a`, live as attachment 7 with a screen. */
function livePinned(): Rig {
  const machine = new AttachmentMachine(TERM, 'core-a');
  const feed: Rig['feed'] = (input) => machine.handle(input);
  feed({ kind: 'containerSize', width: 800, height: 340 });
  feed({ kind: 'show' });
  feed({ kind: 'event', event: connectionEvent(true, 'core-a') });
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

describe('attachmentMachine pinned backend instance', () => {
  it('a pinned panel attaches naming its instance once a session of that instance is up', () => {
    const machine = new AttachmentMachine(TERM, 'core-a');
    machine.handle({ kind: 'show' });
    const effects = machine.handle({ kind: 'event', event: connectionEvent(true, 'core-a') });
    expect(opsOf(effects)).toMatchObject([{ kind: 'attach', terminal: TERM, instance: 'core-a' }]);
  });

  it('a pinned panel sends nothing to a session of another instance and reads as gone', () => {
    const machine = new AttachmentMachine(TERM, 'core-a');
    machine.handle({ kind: 'show' });
    expect(opsOf(machine.handle({ kind: 'event', event: connectionEvent(true, 'core-b') }))).toEqual([]);
    expect(opsOf(machine.handle({ kind: 'event', event: connectionEvent(true, null) }))).toEqual([]);
    expect(machine.view()).toMatchObject({
      phase: 'waiting',
      connected: false,
      status: { kind: 'gone', why: 'other-instance' },
    });
    machine.handle({ kind: 'hide' });
    machine.handle({ kind: 'show' });
    expect(machine.view().status).toEqual({ kind: 'gone', why: 'other-instance' });
    machine.handle({ kind: 'event', event: connectionEvent(false) });
    const effects = machine.handle({ kind: 'event', event: connectionEvent(true, 'core-a') });
    expect(opsOf(effects)).toMatchObject([{ kind: 'attach', instance: 'core-a' }]);
    expect(machine.view().status).toBeNull();
  });

  it('a live pinned panel resumes naming its instance after a reconnect to the same instance', () => {
    const t = livePinned();
    t.feed({ kind: 'event', event: connectionEvent(false) });
    const effects = t.feed({ kind: 'event', event: connectionEvent(true, 'core-a') });
    expect(opsOf(effects)).toEqual([{ kind: 'resume', cursor: cursor(7, 1, 0), instance: 'core-a' }]);
  });

  it('a live pinned panel keeps its screen and sends nothing after a reconnect to another instance', () => {
    const t = livePinned();
    t.feed({ kind: 'event', event: connectionEvent(false) });
    const effects = t.feed({ kind: 'event', event: connectionEvent(true, 'core-b') });
    expect(opsOf(effects)).toEqual([]);
    expect(effects.filter((effect) => effect.kind === 'disposeInstance')).toEqual([]);
    expect(t.machine.view()).toMatchObject({
      hasInstance: true,
      status: { kind: 'gone', why: 'other-instance' },
    });
  });

  it('backend_mismatch answering an attach ends the panel as gone, without another attach', () => {
    const machine = new AttachmentMachine(TERM, 'core-a');
    machine.handle({ kind: 'show' });
    machine.handle({ kind: 'event', event: connectionEvent(true, 'core-a') });
    const effects = machine.handle({
      kind: 'event',
      event: errorEvent({ code: 'backend_mismatch', request: 1 }),
    });
    expect(opsOf(effects)).toEqual([]);
    expect(machine.view()).toMatchObject({
      phase: 'ended',
      status: { kind: 'gone', why: 'other-instance' },
    });
    machine.handle({ kind: 'event', event: connectionEvent(false) });
    const again = machine.handle({ kind: 'event', event: connectionEvent(true, 'core-a') });
    expect(opsOf(again)).toMatchObject([{ kind: 'attach', instance: 'core-a' }]);
  });

  it('backend_mismatch answering a resume keeps the screen and does not attach fresh', () => {
    const t = livePinned();
    t.feed({ kind: 'event', event: connectionEvent(false) });
    t.feed({ kind: 'event', event: connectionEvent(true, 'core-a') });
    const effects = t.feed({
      kind: 'event',
      event: errorEvent({ code: 'backend_mismatch', request: 2 }),
    });
    expect(opsOf(effects)).toEqual([]);
    expect(t.machine.view()).toMatchObject({
      phase: 'ended',
      hasInstance: true,
      stale: true,
      status: { kind: 'gone', why: 'other-instance' },
    });
  });

  it('a panel that follows the active terminal names no instance and attaches on any session', () => {
    const machine = new AttachmentMachine(TERM);
    machine.handle({ kind: 'show' });
    const effects = machine.handle({ kind: 'event', event: connectionEvent(true, 'core-b') });
    expect(opsOf(effects)).toMatchObject([{ kind: 'attach', instance: null }]);
  });
});
