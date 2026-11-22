import { describe, expect, it } from 'vitest';
import { buildPaletteEntries, runPaletteAction } from '../../src/shell/paletteEntries';
import { HOST_A, HOST_B, host, snapshot, space, terminal } from '../support/snapshots';

const connected = { kind: 'connected' } as const;
const failed = { kind: 'failed', message: 'boom', detail: null } as const;

function sample() {
  return snapshot([
    host('mac', HOST_A, connected, [space(HOST_A, '/src/alpha', [terminal('t1', 'first')])]),
    host('mini', HOST_B, failed, [space(HOST_B, '/src/mimir', [terminal('t2', 'remote')])]),
    host('bare', null, { kind: 'connecting' }),
  ]);
}

describe('paletteEntries', () => {
  it('lists spaces and terminals from every host', () => {
    const actions = buildPaletteEntries(sample()).map((entry) => entry.action);
    expect(actions).toContainEqual({ kind: 'switch-space', space: { hostId: HOST_A, path: '/src/alpha' } });
    expect(actions).toContainEqual({ kind: 'switch-space', space: { hostId: HOST_B, path: '/src/mimir' } });
    expect(actions).toContainEqual({
      kind: 'focus-terminal',
      space: { hostId: HOST_A, path: '/src/alpha' },
      terminalId: 't1',
    });
    expect(actions).toContainEqual({
      kind: 'focus-terminal',
      space: { hostId: HOST_B, path: '/src/mimir' },
      terminalId: 't2',
    });
  });

  it('retry keyed by host name for failed hosts only', () => {
    const retries = buildPaletteEntries(sample()).filter((e) => e.action.kind === 'retry-host');
    expect(retries.map((entry) => entry.label)).toEqual(['Retry mini']);
    expect(retries[0]?.action).toEqual({ kind: 'retry-host', hostName: 'mini' });
    const calls: string[] = [];
    runPaletteAction(retries[0]!.action, {
      onSelectSpace: () => {},
      onSelectTerminal: () => {},
      onRetryHost: (name) => calls.push(name),
    });
    expect(calls).toEqual(['mini']);
  });

  it('no new-terminal or open-folder entries', () => {
    const entries = buildPaletteEntries(sample());
    expect(entries.every((e) => ['switch-space', 'focus-terminal', 'retry-host'].includes(e.action.kind))).toBe(true);
    expect(entries.some((e) => /new terminal|open folder/i.test(e.label))).toBe(false);
  });
});
