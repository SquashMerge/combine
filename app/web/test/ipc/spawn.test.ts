import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createTauriSpawnPort } from '../../src/ipc/spawn';
import type { HostId } from '../../src/model/ids';
import { SpawnError } from '../../src/model/spawn';

const { invoke, listen } = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen }));

const fixture = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'spawn-event.json'), 'utf8'),
) as Record<string, Record<string, unknown>>;
const hostId = 'host-1' as HostId;
const HASH = '9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08';

type Handler = (event: { payload: unknown }) => void;
let handler: Handler | null;
let order: string[];

beforeEach(() => {
  invoke.mockReset();
  listen.mockReset();
  handler = null;
  order = [];
  listen.mockImplementation(async (name: string, h: Handler) => {
    order.push(`listen:${name}`);
    handler = h;
    await new Promise((r) => setTimeout(r, 5));
    order.push('listen-resolved');
    return () => {};
  });
});

function reply(payload: unknown): void {
  if (handler === null) throw new Error('no listener');
  handler({ payload });
}

describe('createTauriSpawnPort', () => {
  it('listener_registered_before_invoke', async () => {
    invoke.mockImplementation(async (cmd: string) => {
      order.push(`invoke:${cmd}`);
      queueMicrotask(() => reply({ ...fixture['spawned'], request: '8' }));
      return '8';
    });
    const port = createTauriSpawnPort();
    expect(await port.spawnTerminal(hostId, '/work', { kind: 'shell' })).toBe('hc:t12');
    await port.spawnTerminal(hostId, '/work', { kind: 'shell' });
    expect(order).toEqual([
      'listen:spawn-reply',
      'listen-resolved',
      'invoke:spawn_terminal',
      'invoke:spawn_terminal',
    ]);
    expect(invoke).toHaveBeenCalledWith('spawn_terminal', {
      hostId,
      space: '/work',
      source: { kind: 'shell' },
    });
  });

  it('reply_before_invoke_resolves_is_delivered', async () => {
    invoke.mockImplementation(async () => {
      reply({ ...fixture['spawned'], request: '9007199254740993' });
      return '9007199254740993';
    });
    const port = createTauriSpawnPort();
    const source = { kind: 'template', panel: 'claude', hash: HASH } as const;
    expect(await port.spawnTerminal(hostId, '/work', source)).toBe('hc:t12');
    expect(invoke).toHaveBeenCalledWith('spawn_terminal', {
      hostId,
      space: '/work',
      source: (fixture['sources'] as unknown as unknown[])[1],
    });
    expect(invoke).toHaveBeenCalledWith('spawn_terminal', { hostId, space: '/work', source });
  });

  it('lists templates for the matching request only', async () => {
    invoke.mockImplementation(async () => {
      reply({ ...fixture['templates'], request: '5' });
      reply(fixture['templates']);
      return '9007199254740993';
    });
    const list = await createTauriSpawnPort().listTemplates(hostId, '/work');
    expect(list.templates[0]?.panel).toBe('claude');
    expect(invoke).toHaveBeenCalledWith('list_templates', { hostId, space: '/work' });
  });

  it('error_event_rejects_with_code', async () => {
    invoke.mockImplementation(async () => {
      reply(fixture['error']);
      return '9';
    });
    const error = await createTauriSpawnPort()
      .spawnTerminal(hostId, '/work', { kind: 'shell' })
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(SpawnError);
    expect((error as SpawnError).code).toBe('cwd_refused');
  });

  it('template_changed_rejects_with_its_code_and_message', async () => {
    invoke.mockImplementation(async () => {
      reply(fixture['changed']);
      return '10';
    });
    const error = await createTauriSpawnPort()
      .spawnTerminal(hostId, '/work', { kind: 'template', panel: 'claude', hash: HASH })
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(SpawnError);
    expect(error).toMatchObject({
      code: 'template_changed',
      message: 'the panel claude changed since it was listed; review it and choose again',
    });
  });

  it('rejects a reply of the wrong kind', async () => {
    invoke.mockImplementation(async () => {
      reply({ ...fixture['templates'], request: '9' });
      return '9';
    });
    const error = await createTauriSpawnPort()
      .spawnTerminal(hostId, '/work', { kind: 'shell' })
      .catch((e: unknown) => e);
    expect(error).toMatchObject({ code: null, message: 'Unexpected reply from the host.' });
  });

  it('turns a rejected invoke into a SpawnError', async () => {
    invoke.mockRejectedValue('not connected');
    const error = await createTauriSpawnPort()
      .listTemplates(hostId, '/work')
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(SpawnError);
    expect(error).toMatchObject({ code: null, message: 'not connected' });
  });

  it('rejects a request id that is not a decimal string', async () => {
    invoke.mockResolvedValue(7);
    await expect(createTauriSpawnPort().listTemplates(hostId, '/work')).rejects.toBeInstanceOf(
      SpawnError,
    );
  });
});
