import { describe, expect, it } from 'vitest';
import type { DecimalU64 } from '../../src/model/ids';
import type { LayoutOp, LayoutSnapshot } from '../../src/model/layout';
import { loadLayoutEventFixture } from '../support/memoryLayoutPort';
import { LayoutQueue, STALE_NOTICE } from '../../src/model/layoutQueue';

function snap(revision: string, tiles: string[] = ['t1', 't2']): LayoutSnapshot {
  const [first = 't1', ...rest] = tiles;
  let root: LayoutSnapshot['root'] = { type: 'tile', id: first, panels: [], active: null };
  for (const id of rest) {
    root = {
      type: 'split',
      dir: 'h',
      ratio: 0.5,
      a: root,
      b: { type: 'tile', id, panels: [], active: null },
    };
  }
  return {
    space: '/work',
    layoutRevision: revision as DecimalU64,
    root,
    readOnly: false,
    error: null,
    dropped: null,
  };
}

const rid = (n: number) => String(n) as DecimalU64;
/** Valid opaque tile IDs that name properties every plain object inherits. */
const INHERITED = ['__proto__', 'constructor', 'toString', 'hasOwnProperty'];
const setActive = (tile: string): LayoutOp[] => [{ op: 'set_active', tile, panel: 'p' }];
const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function setup(newTileId?: () => string) {
  const sent: { base: string; ops: LayoutOp[]; request: DecimalU64 }[] = [];
  const notices: string[] = [];
  let next = 1;
  const queue = new LayoutQueue({
    send: (base, ops) => {
      const request = rid(next++);
      sent.push({ base, ops, request });
      return Promise.resolve(request);
    },
    onNotice: (message) => notices.push(message),
    ...(newTileId === undefined ? {} : { newTileId }),
  });
  return { queue, sent, notices };
}

describe('LayoutQueue', () => {
  it('one mutation in flight per space', async () => {
    const { queue, sent } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    queue.enqueue(setActive('t2'));
    await flush();
    expect(sent.map((s) => s.base)).toEqual(['5']);
    queue.onSnapshot(rid(1), snap('6'));
    await flush();
    expect(sent.map((s) => s.base)).toEqual(['5', '6']);
    expect(sent[1]?.ops).toEqual(setActive('t2'));
  });

  it('holds intents until a snapshot is held', async () => {
    const { queue, sent } = setup();
    queue.enqueue(setActive('t1'));
    await flush();
    expect(sent).toEqual([]);
    queue.onSnapshot(rid(99), snap('3'));
    await flush();
    expect(sent.map((s) => s.base)).toEqual(['3']);
  });

  it('uses a reply that arrives before the send resolves', async () => {
    const { queue, sent } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    queue.enqueue(setActive('t2'));
    queue.onSnapshot(rid(1), snap('6'));
    await flush();
    expect(sent.map((s) => s.base)).toEqual(['5', '6']);
  });

  it('stale after newer push resends immediately', async () => {
    const { queue, sent, notices } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    await flush();
    queue.onSnapshot(null, snap('6'));
    queue.onError(rid(1), 'stale_layout', 'stale');
    await flush();
    expect(sent.map((s) => s.base)).toEqual(['5', '6']);
    expect(notices).toEqual([]);
  });

  it('stale before newer push waits then resends', async () => {
    const { queue, sent, notices } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    await flush();
    queue.onError(rid(1), 'stale_layout', 'stale');
    await flush();
    expect(sent).toHaveLength(1);
    queue.onSnapshot(null, snap('5'));
    await flush();
    expect(sent).toHaveLength(1);
    queue.onSnapshot(null, snap('6'));
    await flush();
    expect(sent.map((s) => s.base)).toEqual(['5', '6']);
    expect(notices).toEqual([]);
  });

  it('second stale drops intent with notice', async () => {
    const { queue, sent, notices } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    queue.enqueue(setActive('t2'));
    await flush();
    queue.onSnapshot(null, snap('6'));
    queue.onError(rid(1), 'stale_layout', 'stale');
    await flush();
    queue.onSnapshot(null, snap('7'));
    queue.onError(rid(2), 'stale_layout', 'stale');
    await flush();
    expect(notices).toEqual([STALE_NOTICE]);
    expect(sent.map((s) => s.ops[0])).toEqual([...setActive('t1'), ...setActive('t1'), ...setActive('t2')]);
    expect(sent.map((s) => s.base)).toEqual(['5', '6', '7']);
  });

  it('drops a stale intent whose tile is gone', async () => {
    const { queue, sent, notices } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t2'));
    await flush();
    queue.onSnapshot(null, snap('6', ['t1']));
    queue.onError(rid(1), 'stale_layout', 'stale');
    await flush();
    expect(sent).toHaveLength(1);
    expect(notices).toEqual([STALE_NOTICE]);
  });

  it('invalid_request drops intent', async () => {
    const { queue, sent, notices } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    queue.enqueue(setActive('t2'));
    await flush();
    queue.onError(rid(1), 'invalid_request', 'no such tile');
    await flush();
    expect(notices).toEqual(['no such tile']);
    expect(sent.map((s) => s.ops[0])).toEqual([...setActive('t1'), ...setActive('t2')]);
  });

  it('disconnect clears queue', async () => {
    const { queue, sent } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    queue.enqueue(setActive('t2'));
    await flush();
    queue.onDisconnected();
    queue.onSnapshot(rid(1), snap('6'));
    await flush();
    expect(sent).toHaveLength(1);
  });

  it('drops the queue and says why when a send is refused', async () => {
    const notices: string[] = [];
    const queue = new LayoutQueue({
      send: () => Promise.reject('not connected'),
      onNotice: (message) => notices.push(message),
    });
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    await flush();
    expect(notices).toEqual(['not connected']);
  });

  it('builds split and close ops with an injectable new tile id', async () => {
    const { queue, sent } = setup(() => 't-new');
    queue.onSnapshot(null, snap('5'));
    queue.split('t1', 'v');
    await flush();
    queue.onSnapshot(rid(1), snap('6'));
    queue.close('t2');
    await flush();
    expect(sent.map((s) => s.ops)).toEqual([
      [{ op: 'split_tile', tile: 't1', dir: 'v', newTile: 't-new', ratio: 0.5, panel: null }],
      [{ op: 'close_tile', tile: 't2' }],
    ]);
  });

  it('defaults the new tile id to t- plus a uuid', async () => {
    const { queue, sent } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.split('t1', 'h');
    await flush();
    const op = sent[0]?.ops[0];
    expect(op?.op === 'split_tile' && op.newTile).toMatch(/^t-[0-9a-f-]{36}$/);
  });

  it('does not send a later intent while an earlier one waits after a stale reply', async () => {
    const { queue, sent } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    await flush();
    queue.onError(rid(1), 'stale_layout', 'stale');
    queue.enqueue(setActive('t2'));
    await flush();
    expect(sent).toHaveLength(1);
    queue.onSnapshot(null, snap('6'));
    await flush();
    expect(sent.map((s) => [s.base, s.ops[0]])).toEqual([
      ['5', setActive('t1')[0]],
      ['6', setActive('t1')[0]],
    ]);
    queue.onSnapshot(rid(2), snap('7'));
    await flush();
    expect(sent.map((s) => [s.base, s.ops[0]])).toEqual([
      ['5', setActive('t1')[0]],
      ['6', setActive('t1')[0]],
      ['7', setActive('t2')[0]],
    ]);
  });

  it('a reply for another request does not release the flight', async () => {
    const { queue, sent } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    queue.enqueue(setActive('t2'));
    await flush();
    queue.onSnapshot(rid(40), snap('6'));
    queue.onError(rid(41), 'invalid_request', 'other');
    await flush();
    expect(sent).toHaveLength(1);
    queue.onSnapshot(rid(1), snap('7'));
    await flush();
    expect(sent).toHaveLength(2);
  });

  it('still sends a later intent after a send was rejected', async () => {
    const bases: string[] = [];
    let fail = true;
    const queue = new LayoutQueue({
      send: (base) => {
        bases.push(base);
        return fail ? Promise.reject(new Error('not connected')) : Promise.resolve(rid(1));
      },
      onNotice: () => undefined,
    });
    queue.onSnapshot(null, snap('5'));
    queue.enqueue(setActive('t1'));
    await flush();
    fail = false;
    queue.enqueue(setActive('t2'));
    await flush();
    expect(bases).toEqual(['5', '5']);
  });

  it('builds the op shapes of the shared ops fixture', async () => {
    const [, , , split, close] = loadLayoutEventFixture().ops;
    const { queue, sent } = setup(() => 't3');
    queue.onSnapshot(null, snap('5', ['t1', 't2', 't3']));
    queue.split('t1', 'v', 'p1');
    await flush();
    queue.onSnapshot(rid(1), snap('6', ['t1', 't2', 't3']));
    queue.close('t3');
    await flush();
    expect(sent[0]?.ops[0]).toEqual(split);
    expect(sent[1]?.ops[0]).toEqual(close);
  });

  it('builds the add_panel and remove_panel ops', async () => {
    const { queue, sent } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.addPanel('notes');
    await flush();
    queue.onSnapshot(rid(1), snap('6'));
    queue.removePanel('notes');
    await flush();
    expect(sent.map((s) => s.ops)).toEqual([
      [{ op: 'add_panel', panel: 'notes', tile: null }],
      [{ op: 'remove_panel', panel: 'notes' }],
    ]);
  });

  it.each(INHERITED)('sends a split of tile %s', async (id) => {
    const { queue, sent, notices } = setup(() => 't-new');
    queue.onSnapshot(null, snap('5', [id, 't2']));
    queue.split(id, 'v');
    await flush();
    expect(notices).toEqual([]);
    expect(sent).toEqual([
      {
        base: '5',
        request: rid(1),
        ops: [{ op: 'split_tile', tile: id, dir: 'v', newTile: 't-new', ratio: 0.5, panel: null }],
      },
    ]);
  });

  it.each(INHERITED)('drops an intent for tile %s when the layout has no such tile', async (id) => {
    const { queue, sent, notices } = setup();
    queue.onSnapshot(null, snap('5'));
    queue.close(id);
    await flush();
    expect(sent).toEqual([]);
    expect(notices).toEqual([STALE_NOTICE]);
  });
});
