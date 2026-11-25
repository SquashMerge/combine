import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DecimalU64 } from '../../src/model/ids';
import {
  EARLY_PANEL_REPLY_LIMIT,
  PANEL_FETCH_TIMEOUT_MS,
  type PanelContent,
  PanelFetchError,
  PanelReplies,
  type PanelsChanged,
} from '../../src/model/panels';

const req = (n: number) => String(n) as DecimalU64;
const content: PanelContent = {
  space: '/work',
  id: 'p',
  revision: '1' as DecimalU64,
  renderer: 'markdown',
  source: '# p',
  state: 'validated',
  error: null,
};
const panel = (request: number, hostId = 'h1'): PanelsChanged => ({ kind: 'panel', hostId, request: req(request), panel: content });
const failure = (request: number): PanelsChanged => ({
  kind: 'error',
  hostId: 'h1',
  request: req(request),
  code: 'unknown_panel',
  message: 'no such panel',
});

const done = (kind: 'closed' | 'pinned', request: number, hostId = 'h1'): PanelsChanged => ({
  kind,
  hostId,
  request: req(request),
  space: '/work',
  panel: 'p',
});

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe('PanelReplies', () => {
  it('resolves a wait with the reply that follows it', async () => {
    const replies = new PanelReplies();
    const wait = replies.wait('h1', req(1));
    replies.onEvent(panel(1));
    await expect(wait).resolves.toBe(content);
  });

  it('answers a wait from a reply that came first', async () => {
    const replies = new PanelReplies();
    replies.onEvent(panel(1));
    await expect(replies.wait('h1', req(1))).resolves.toBe(content);
  });

  it('rejects with the code and message of an error', async () => {
    const replies = new PanelReplies();
    const wait = replies.wait('h1', req(2));
    replies.onEvent(failure(2));
    await expect(wait).rejects.toMatchObject({ code: 'unknown_panel', message: 'no such panel' });
  });

  it('rejects an early error when it is waited for', async () => {
    const replies = new PanelReplies();
    replies.onEvent(failure(2));
    await expect(replies.wait('h1', req(2))).rejects.toBeInstanceOf(PanelFetchError);
  });

  it('ignores list events', async () => {
    const replies = new PanelReplies();
    const wait = replies.wait('h1', req(1));
    replies.onEvent({
      kind: 'list',
      hostId: 'h1',
      request: req(1),
      list: { space: '/work', panels: [], skipped: [], error: null },
    });
    replies.onEvent(panel(1));
    await expect(wait).resolves.toBe(content);
  });

  it('waitDone resolves closed and pinned and rejects errors and panel replies', async () => {
    const replies = new PanelReplies();
    const closed = replies.waitDone('h1', req(1));
    replies.onEvent(done('closed', 1));
    await expect(closed).resolves.toBeUndefined();
    replies.onEvent(done('pinned', 2));
    await expect(replies.waitDone('h1', req(2))).resolves.toBeUndefined();

    const refused = replies.waitDone('h1', req(3));
    replies.onEvent(failure(3));
    await expect(refused).rejects.toMatchObject({ code: 'unknown_panel', message: 'no such panel' });
    replies.onEvent(failure(4));
    await expect(replies.waitDone('h1', req(4))).rejects.toMatchObject({ code: 'unknown_panel' });

    const content = replies.waitDone('h1', req(5));
    replies.onEvent(panel(5));
    await expect(content).rejects.toMatchObject({ code: null, message: 'The host sent an unexpected reply.' });
    replies.onEvent(panel(6));
    await expect(replies.waitDone('h1', req(6))).rejects.toMatchObject({ code: null });
  });

  it('wait rejects a closed or pinned reply as unexpected', async () => {
    const replies = new PanelReplies();
    const wait = replies.wait('h1', req(1));
    replies.onEvent(done('closed', 1));
    await expect(wait).rejects.toMatchObject({ code: null, message: 'The host sent an unexpected reply.' });
    replies.onEvent(done('pinned', 2));
    await expect(replies.wait('h1', req(2))).rejects.toMatchObject({ code: null });
  });

  it('a disconnect rejects a waitDone', async () => {
    const replies = new PanelReplies();
    const wait = replies.waitDone('h1', req(1));
    replies.onEvent({ kind: 'disconnected', hostId: 'h1' });
    await expect(wait).rejects.toMatchObject({ message: 'The host disconnected.' });
  });

  it('keeps the newest 32 early replies', async () => {
    const replies = new PanelReplies();
    for (let n = 1; n <= EARLY_PANEL_REPLY_LIMIT + 1; n += 1) replies.onEvent(panel(n));
    const dropped = replies.wait('h1', req(1));
    const kept = replies.wait('h1', req(EARLY_PANEL_REPLY_LIMIT + 1));
    await expect(kept).resolves.toBe(content);
    vi.advanceTimersByTime(PANEL_FETCH_TIMEOUT_MS);
    await expect(dropped).rejects.toThrow('No reply from the host within 30 s.');
  });

  it('a disconnect rejects only that host and drops its early replies', async () => {
    const replies = new PanelReplies();
    const mine = replies.wait('h1', req(1));
    const other = replies.wait('h2', req(1));
    replies.onEvent(panel(5));
    replies.onEvent({ kind: 'disconnected', hostId: 'h1' });
    await expect(mine).rejects.toMatchObject({ code: null, message: 'The host disconnected.' });
    replies.onEvent(panel(1, 'h2'));
    await expect(other).resolves.toBe(content);
    const afterwards = replies.wait('h1', req(5));
    vi.advanceTimersByTime(PANEL_FETCH_TIMEOUT_MS);
    await expect(afterwards).rejects.toThrow('No reply');
  });

  it('times out after 30 s', async () => {
    const replies = new PanelReplies();
    const wait = replies.wait('h1', req(1));
    vi.advanceTimersByTime(PANEL_FETCH_TIMEOUT_MS - 1);
    replies.onEvent(panel(1));
    await expect(wait).resolves.toBe(content);
    const late = replies.wait('h1', req(2));
    vi.advanceTimersByTime(PANEL_FETCH_TIMEOUT_MS);
    await expect(late).rejects.toMatchObject({ code: null, message: 'No reply from the host within 30 s.' });
  });

  it('settles a wait that a second wait for the same request displaces', async () => {
    const replies = new PanelReplies();
    const first = replies.wait('h1', req(1));
    const second = replies.wait('h1', req(1));
    await expect(first).rejects.toBeInstanceOf(PanelFetchError);
    replies.onEvent(panel(1));
    await expect(second).resolves.toBe(content);
    expect(vi.getTimerCount()).toBe(0);
  });
});
