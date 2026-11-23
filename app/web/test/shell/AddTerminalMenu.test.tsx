import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { TermId } from '../../src/model/ids';
import type { SpaceKey } from '../../src/model/space';
import type { TemplateList, TerminalTemplate } from '../../src/model/spawn';
import { AddTerminalMenu } from '../../src/shell/AddTerminalMenu';
import { MemorySpawnPort } from '../support/memorySpawnPort';
import { HOST_A, HOST_B, host, snapshot, space } from '../support/snapshots';

const alpha: SpaceKey = { hostId: HOST_A, path: '/src/alpha' };
const beta: SpaceKey = { hostId: HOST_A, path: '/src/beta' };
const connected = { kind: 'connected' } as const;

const hosts = snapshot([
  host('mac', HOST_A, connected, [space(HOST_A, alpha.path), space(HOST_A, beta.path)]),
  host('mini', HOST_B, { kind: 'failed', message: 'boom', detail: null }, [
    space(HOST_B, '/src/mimir'),
  ]),
]);

function template(over: Partial<TerminalTemplate> = {}): TerminalTemplate {
  return {
    panel: 'claude',
    title: 'Claude',
    slot: 'claude',
    argv: ['claude', '--resume'],
    env: { FOO: 'bar' } as TerminalTemplate['env'],
    cwd: '',
    ...over,
  };
}

function list(over: Partial<TemplateList> = {}): TemplateList {
  return { space: alpha.path, templates: [], skipped: [], error: null, ...over };
}

function mount(port: MemorySpawnPort, selected: SpaceKey | null = alpha) {
  const onSpawned = vi.fn();
  const view = render(
    <AddTerminalMenu port={port} snapshot={hosts} selected={selected} onSpawned={onSpawned} />,
  );
  const again = (next: SpaceKey | null) =>
    view.rerender(
      <AddTerminalMenu port={port} snapshot={hosts} selected={next} onSpawned={onSpawned} />,
    );
  return { onSpawned, rerender: again };
}

const button = () => screen.getByRole('button', { name: 'Add terminal' });
const menu = () => screen.queryByRole('menu');
const items = () => within(screen.getByRole('menu')).getAllByRole('menuitem');

async function open() {
  await act(async () => {
    fireEvent.click(button());
  });
}

async function click(name: RegExp | string) {
  await act(async () => {
    fireEvent.click(screen.getByRole('menuitem', { name }));
  });
}

describe('AddTerminalMenu', () => {
  it('lists_shell_then_templates_with_command_line_and_cwd', async () => {
    const port = new MemorySpawnPort();
    port.setList(
      list({
        templates: [
          template({ cwd: 'my dir' }),
          template({ panel: 'bare', title: null, slot: 'slot-name', argv: ['bash'], env: {} as TerminalTemplate['env'] }),
        ],
      }),
    );
    mount(port);
    await open();
    const rows = items();
    expect(rows).toHaveLength(3);
    expect(rows[0]?.textContent).toContain('Shell in alpha');
    expect(rows[1]?.textContent).toContain('Claude');
    expect(rows[1]?.querySelector('code.command')?.textContent).toBe('FOO=bar claude --resume');
    expect(rows[1]?.textContent).toContain('in "my dir"');
    expect(rows[2]?.textContent).toContain('slot-name');
    expect(rows[2]?.textContent).not.toContain(' in ');
    expect(button().getAttribute('aria-expanded')).toBe('true');
  });

  it('shows_command_line_exactly_without_ellipsis', async () => {
    const port = new MemorySpawnPort();
    const long = 'x'.repeat(300);
    port.setList(list({ templates: [template({ argv: ['run', long], env: {} as TerminalTemplate['env'] })] }));
    mount(port);
    await open();
    const code = screen.getByRole('menu').querySelector('code.command');
    expect(code?.textContent).toBe(`run ${long}`);
    expect(code?.textContent).not.toContain('…');
  });

  it('shows_skipped_panels_with_reasons', async () => {
    const port = new MemorySpawnPort();
    port.setList(list({ skipped: [{ panel: 'notes', reason: 'not a terminal panel' }] }));
    mount(port);
    await open();
    const skipped = screen.getByRole('menuitem', { name: 'notes: not a terminal panel' });
    expect(skipped.getAttribute('aria-disabled')).toBe('true');
    fireEvent.click(skipped);
    expect(port.spawnCalls()).toHaveLength(0);
  });

  it('shows_list_error_and_rejection', async () => {
    const port = new MemorySpawnPort();
    port.setList(list({ error: 'panels folder unreadable' }));
    mount(port);
    await open();
    expect(screen.getByRole('alert').textContent).toBe('panels folder unreadable');

    const failing = new MemorySpawnPort();
    failing.failList('host went away');
    cleanup();
    mount(failing);
    await open();
    expect(
      screen.getByText("Couldn't list templates: host went away").getAttribute('role'),
    ).toBe('alert');
  });

  it('loading_state', async () => {
    const port = new MemorySpawnPort();
    port.deferList();
    mount(port);
    await open();
    expect(screen.getByText('Loading templates…')).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: /Shell/ })).toBeTruthy();
    await act(async () => {
      port.resolveList(list({ templates: [template()] }));
    });
    expect(screen.queryByText('Loading templates…')).toBeNull();
    expect(screen.getByRole('menuitem', { name: /Claude/ })).toBeTruthy();
  });

  it('opening_lists_templates_only', async () => {
    const port = new MemorySpawnPort();
    mount(port);
    expect(port.calls).toHaveLength(0);
    await open();
    expect(port.listCalls()).toEqual([
      { method: 'listTemplates', hostId: HOST_A, space: '/src/alpha' },
    ]);
    expect(port.spawnCalls()).toHaveLength(0);
    fireEvent.keyDown(screen.getByRole('menu'), { key: 'Escape' });
    expect(menu()).toBeNull();
    expect(port.spawnCalls()).toHaveLength(0);
    await open();
    expect(port.listCalls()).toHaveLength(2);
  });

  it('shell_click_sends_shell_source', async () => {
    const port = new MemorySpawnPort();
    mount(port);
    await open();
    await click(/^Shell/);
    expect(port.spawnCalls()).toEqual([
      { method: 'spawnTerminal', hostId: HOST_A, space: '/src/alpha', source: { kind: 'shell' } },
    ]);
  });

  it('template_click_sends_panel', async () => {
    const port = new MemorySpawnPort();
    port.setList(list({ templates: [template()] }));
    mount(port);
    await open();
    await click(/Claude/);
    expect(port.spawnCalls().map((c) => c.source)).toEqual([{ kind: 'template', panel: 'claude' }]);
  });

  it('one_spawn_per_click', async () => {
    const port = new MemorySpawnPort();
    port.setList(list({ templates: [template()] }));
    mount(port);
    await open();
    await click(/Claude/);
    expect(port.spawnCalls()).toHaveLength(1);
  });

  it('clicks_while_starting_send_nothing_more', async () => {
    const port = new MemorySpawnPort();
    port.setList(list({ templates: [template()] }));
    port.deferSpawn();
    mount(port);
    await open();
    await click(/Claude/);
    expect(port.spawnCalls()).toHaveLength(1);
    for (const item of items()) expect((item as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText('Starting…')).toBeTruthy();
    await act(async () => {
      for (const item of items()) fireEvent.click(item);
    });
    expect(port.spawnCalls()).toHaveLength(1);
  });

  it('success_calls_onSpawned_and_closes', async () => {
    const port = new MemorySpawnPort();
    port.setSpawned('hc:t9' as TermId);
    const { onSpawned } = mount(port);
    await open();
    await click(/^Shell/);
    expect(onSpawned).toHaveBeenCalledExactlyOnceWith(alpha, 'hc:t9');
    expect(menu()).toBeNull();
  });

  it('refusal_shows_reason_inline', async () => {
    const port = new MemorySpawnPort();
    port.failSpawn('the template\'s cwd "../x" has a .. segment');
    const { onSpawned } = mount(port);
    await open();
    await click(/^Shell/);
    expect(screen.getByRole('alert').textContent).toBe(
      'Couldn\'t add the terminal: the template\'s cwd "../x" has a .. segment',
    );
    expect(menu()).not.toBeNull();
    expect(onSpawned).not.toHaveBeenCalled();
    for (const item of items()) expect((item as HTMLButtonElement).disabled).toBe(false);
  });

  it('disabled_when_host_disconnected', () => {
    const port = new MemorySpawnPort();
    mount(port, { hostId: HOST_B, path: '/src/mimir' });
    expect((button() as HTMLButtonElement).disabled).toBe(true);
    expect(button().getAttribute('title')).toBe("mini isn't connected");
    fireEvent.click(button());
    expect(menu()).toBeNull();
    expect(port.calls).toHaveLength(0);
  });

  it('disabled_when_no_space_selected', () => {
    mount(new MemorySpawnPort(), null);
    expect((button() as HTMLButtonElement).disabled).toBe(true);
  });

  it('space_change_drops_stale_reply', async () => {
    const port = new MemorySpawnPort();
    port.deferList();
    const { rerender } = mount(port);
    await open();
    rerender(beta);
    expect(menu()).toBeNull();
    await open();
    expect(port.pendingListCount()).toBe(2);
    await act(async () => {
      port.resolveList(list({ templates: [template()] }), 0);
    });
    expect(screen.getByText('Loading templates…')).toBeTruthy();
    expect(screen.queryByRole('menuitem', { name: /Claude/ })).toBeNull();
    await act(async () => {
      port.resolveList(list({ space: beta.path }), 0);
    });
    expect(screen.queryByText('Loading templates…')).toBeNull();
    expect(port.listCalls().map((c) => c.space)).toEqual(['/src/alpha', '/src/beta']);
  });
});
