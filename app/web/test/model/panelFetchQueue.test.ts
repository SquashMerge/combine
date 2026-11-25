import { describe, expect, it } from 'vitest';
import type { DecimalU64, HostId } from '../../src/model/ids';
import { PANEL_FETCHES_IN_FLIGHT, type PanelContent, PanelFetchError, PanelFetchQueue } from '../../src/model/panels';

const A = 'host-a' as HostId;
const B = 'host-b' as HostId;

function content(id: string): PanelContent {
  return { space: '/w', id, revision: '1' as DecimalU64, renderer: 'markdown', source: id, state: 'validated', error: null };
}

interface Sent {
  hostId: HostId;
  panel: string;
  resolve: () => void;
  reject: (e: Error) => void;
}

function harness(limit?: number) {
  const sent: Sent[] = [];
  const queue = new PanelFetchQueue(
    (hostId, _space, panel) =>
      new Promise<PanelContent>((resolve, reject) => {
        sent.push({ hostId, panel, resolve: () => resolve(content(panel)), reject });
      }),
    limit,
  );
  return { sent, queue };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

describe('PanelFetchQueue', () => {
  it('at_most_four_are_in_flight_per_host', async () => {
    expect(PANEL_FETCHES_IN_FLIGHT).toBe(4);
    const { sent, queue } = harness();
    const calls = Array.from({ length: 10 }, (_, n) => queue.fetch(A, '/w', `p${n}`, false));
    expect(sent.map((s) => s.panel)).toEqual(['p0', 'p1', 'p2', 'p3']);
    expect(queue.inFlight(A)).toBe(4);
    sent[0]?.resolve();
    await tick();
    expect(sent.map((s) => s.panel)).toEqual(['p0', 'p1', 'p2', 'p3', 'p4']);
    expect(queue.inFlight(A)).toBe(4);
    for (let n = 1; n < 10; n += 1) {
      sent[n]?.resolve();
      await tick();
    }
    await Promise.all(calls);
    expect(sent).toHaveLength(10);
    expect(queue.inFlight(A)).toBe(0);
  });

  it('counts each host on its own', () => {
    const { sent, queue } = harness();
    for (let n = 0; n < 6; n += 1) void queue.fetch(A, '/w', `a${n}`, false);
    void queue.fetch(B, '/w', 'b0', false);
    expect(sent.filter((s) => s.hostId === A)).toHaveLength(4);
    expect(sent.filter((s) => s.hostId === B)).toHaveLength(1);
  });

  it('a failure frees its slot', async () => {
    const { sent, queue } = harness(1);
    const first = queue.fetch(A, '/w', 'p0', false);
    void queue.fetch(A, '/w', 'p1', false);
    sent[0]?.reject(new Error('boom'));
    await expect(first).rejects.toMatchObject({ message: 'boom' });
    await tick();
    expect(sent.map((s) => s.panel)).toEqual(['p0', 'p1']);
  });

  it('urgent_requests_go_first', async () => {
    const { sent, queue } = harness(1);
    void queue.fetch(A, '/w', 'busy', false);
    void queue.fetch(A, '/w', 'late', false);
    void queue.fetch(A, '/w', 'urgent1', true);
    void queue.fetch(A, '/w', 'urgent2', true);
    for (let n = 0; n < 3; n += 1) {
      sent[n]?.resolve();
      await tick();
    }
    expect(sent.map((s) => s.panel)).toEqual(['busy', 'urgent1', 'urgent2', 'late']);
  });

  it('a_newer_request_replaces_the_waiting_one', async () => {
    const { sent, queue } = harness(1);
    void queue.fetch(A, '/w', 'busy', false);
    const old = queue.fetch(A, '/w', 'p', true);
    void queue.fetch(A, '/w', 'other', false);
    const replacement = queue.fetch(A, '/w', 'p', false);
    await expect(old).rejects.toMatchObject({ code: 'superseded', message: 'A newer request replaced this one.' });
    sent[0]?.resolve();
    await tick();
    // The replacement keeps the old call's place, and is urgent because the old one was.
    expect(sent.map((s) => s.panel)).toEqual(['busy', 'p']);
    sent[1]?.resolve();
    await expect(replacement).resolves.toMatchObject({ id: 'p' });
  });

  it('does not replace a call already sent', async () => {
    const { sent, queue } = harness(2);
    const first = queue.fetch(A, '/w', 'p', false);
    const second = queue.fetch(A, '/w', 'p', false);
    expect(sent.map((s) => s.panel)).toEqual(['p', 'p']);
    sent[0]?.resolve();
    sent[1]?.resolve();
    await expect(first).resolves.toBeDefined();
    await expect(second).resolves.toBeDefined();
  });

  it('a_disconnect_rejects_the_waiting_ones', async () => {
    const { sent, queue } = harness(1);
    const running = queue.fetch(A, '/w', 'p0', false);
    const waiting = queue.fetch(A, '/w', 'p1', false);
    const otherHost = queue.fetch(B, '/w', 'q', false);
    queue.disconnected(A);
    await expect(waiting).rejects.toMatchObject({ code: null, message: 'The host disconnected.' });
    await expect(waiting).rejects.toBeInstanceOf(PanelFetchError);
    sent[0]?.reject(new PanelFetchError(null, 'The host disconnected.'));
    await expect(running).rejects.toBeDefined();
    await tick();
    expect(sent.map((s) => s.panel)).toEqual(['p0', 'q']);
    sent[1]?.resolve();
    await expect(otherHost).resolves.toBeDefined();
  });
});
