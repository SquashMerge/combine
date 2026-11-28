import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { TermId } from '../../src/model/ids';
import type { SpaceKey } from '../../src/model/space';
import { SpawnError, type TemplateList, type TerminalTemplate } from '../../src/model/spawn';
import { AddTerminalMenu } from '../../src/shell/AddTerminalMenu';
import { MemorySpawnPort } from '../support/memorySpawnPort';
import { HOST_A, HOST_B, host, snapshot, space } from '../support/snapshots';

const alpha: SpaceKey = { hostId: HOST_A, path: '/src/alpha' };
const beta: SpaceKey = { hostId: HOST_A, path: '/src/beta' };
const connected = { kind: 'connected' } as const;
const HASH_1 = '9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08';
const HASH_2 = '60303ae22b998861bce3b28f33eec1be758a213c86c93c076dbe9f558c11c752';

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
    hash: HASH_1,
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
    expect(rows[0]?.querySelector('.detail code')?.textContent).toBe('alpha');
    expect(rows[1]?.textContent).toContain('Claude');
    expect(rows[1]?.querySelector('code.command')?.textContent).toBe('FOO=bar claude --resume');
    expect(rows[1]?.textContent).toContain('in "my dir"');
    expect(rows[1]?.querySelector('.detail code')?.textContent).toBe('"my dir"');
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

  it('the_approval_line_orders_env_by_code_point', async () => {
    const port = new MemorySpawnPort();
    const env = { b: '1', '10': '2', '2': '3', a: '4' } as TerminalTemplate['env'];
    port.setList(list({ templates: [template({ argv: ['run', 'k=v'], env })] }));
    mount(port);
    await open();
    const code = screen.getByRole('menu').querySelector('code.command');
    expect(code?.textContent).toBe('10=2 2=3 a=4 b=1 run "k=v"');
  });

  it('shows_skipped_panels_with_reasons', async () => {
    const port = new MemorySpawnPort();
    port.setList(list({ skipped: [{ panel: 'notes', reason: 'not a terminal panel' }] }));
    mount(port);
    await open();
    const skipped = screen
      .getAllByRole('menuitem')
      .find((item) => item.textContent === 'notes: not a terminal panel') as HTMLElement;
    expect(skipped.querySelector('code')?.textContent).toBe('notes');
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

  it('escape_closes_a_menu_opened_with_the_mouse', async () => {
    const port = new MemorySpawnPort();
    mount(port);
    await open();
    expect(menu()).not.toBeNull();
    fireEvent.keyDown(document.body, { key: 'Escape' });
    expect(menu()).toBeNull();
    expect(button().getAttribute('aria-expanded')).toBe('false');
    expect(port.spawnCalls()).toHaveLength(0);
  });

  it('escape_listener_is_removed_on_every_close_and_on_unmount', async () => {
    const add = vi.spyOn(document, 'addEventListener');
    const remove = vi.spyOn(document, 'removeEventListener');
    try {
      const port = new MemorySpawnPort();
      const { unmount } = (() => {
        const onSpawned = vi.fn();
        return render(
          <AddTerminalMenu port={port} snapshot={hosts} selected={alpha} onSpawned={onSpawned} />,
        );
      })();
      for (let cycle = 0; cycle < 3; cycle += 1) {
        await open();
        fireEvent.keyDown(document.body, { key: 'Escape' });
        expect(menu()).toBeNull();
      }
      const keydown = (spy: typeof add) =>
        spy.mock.calls.filter(([type]) => type === 'keydown').map(([, handler]) => handler);
      const added = keydown(add);
      expect(added).toHaveLength(3);
      // Every handler that was added has been removed again, closed menu or not.
      expect(keydown(remove)).toEqual(added);
      // Closing without Escape (a second click on the button) also removes it.
      await open();
      expect(keydown(add)).toHaveLength(4);
      fireEvent.click(button());
      expect(keydown(remove)).toHaveLength(4);
      // And unmounting an open menu removes it.
      await open();
      expect(keydown(add)).toHaveLength(5);
      unmount();
      expect(keydown(remove)).toEqual(keydown(add));
    } finally {
      add.mockRestore();
      remove.mockRestore();
    }
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
    expect(port.spawnCalls().map((c) => c.source)).toEqual([
      { kind: 'template', panel: 'claude', hash: HASH_1 },
    ]);
  });

  it('template_click_sends_the_hash_of_the_clicked_entry', async () => {
    const port = new MemorySpawnPort();
    port.setList(
      list({
        templates: [
          template(),
          template({ panel: 'codex', title: 'Codex', slot: 'codex', hash: HASH_2 }),
        ],
      }),
    );
    mount(port);
    await open();
    await click(/Codex/);
    expect(port.spawnCalls().map((c) => c.source)).toEqual([
      { kind: 'template', panel: 'codex', hash: HASH_2 },
    ]);
  });

  it('template_changed_refreshes_the_list_and_does_not_spawn_again', async () => {
    const changed = 'the panel claude changed since it was listed; review it and choose again';
    const port = new MemorySpawnPort();
    port.setList(list({ templates: [template()] }));
    port.failSpawn(new SpawnError('template_changed', changed));
    const { onSpawned } = mount(port);
    await open();
    expect(port.listCalls()).toHaveLength(1);
    // The host now holds an edited definition; the refreshed list shows it with its new hash.
    port.setList(list({ templates: [template({ argv: ['claude', '--edited'], hash: HASH_2 })] }));
    await click(/Claude/);
    expect(screen.getByRole('alert').textContent).toBe(changed);
    expect(menu()).not.toBeNull();
    expect(onSpawned).not.toHaveBeenCalled();
    expect(port.listCalls()).toHaveLength(2);
    expect(port.spawnCalls()).toHaveLength(1);
    expect(screen.getByRole('menu').querySelector('code.command')?.textContent).toBe(
      'FOO=bar claude --edited',
    );
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(port.spawnCalls()).toHaveLength(1);

    port.setSpawned('hc:t7' as TermId);
    await click(/Claude/);
    expect(port.spawnCalls().map((c) => c.source)).toEqual([
      { kind: 'template', panel: 'claude', hash: HASH_1 },
      { kind: 'template', panel: 'claude', hash: HASH_2 },
    ]);
    expect(port.listCalls()).toHaveLength(2);
    expect(onSpawned).toHaveBeenCalledExactlyOnceWith(alpha, 'hc:t7');
  });

  it('other_spawn_errors_do_not_refresh_the_list', async () => {
    const port = new MemorySpawnPort();
    port.setList(list({ templates: [template()] }));
    port.failSpawn(new SpawnError('cwd_refused', 'the template\'s cwd "../x" has a .. segment'));
    mount(port);
    await open();
    await click(/Claude/);
    expect(screen.getByRole('alert').textContent).toBe(
      'Couldn\'t add the terminal: the template\'s cwd "../x" has a .. segment',
    );
    expect(port.listCalls()).toHaveLength(1);
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

  it('space_change_drops_stale_spawn_reply', async () => {
    const port = new MemorySpawnPort();
    port.deferSpawn();
    const { onSpawned, rerender } = mount(port);
    await open();
    await click(/^Shell/);
    rerender(beta);
    await act(async () => {
      port.resolveSpawn('hc:t5' as TermId);
    });
    expect(onSpawned).not.toHaveBeenCalled();
    expect(menu()).toBeNull();
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
