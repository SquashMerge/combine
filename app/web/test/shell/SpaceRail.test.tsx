import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { spaceKeyId } from '../../src/model/space';
import { App } from '../../src/shell/App';
import { SpaceRail } from '../../src/shell/SpaceRail';
import { MemoryHostsPort } from '../support/memoryHostsPort';
import { MemoryLayoutPort } from '../support/memoryLayoutPort';
import { MemoryTerminalPort } from '../support/memoryTerminalPort';
import { MemoryClientStatePort } from '../support/memoryClientStatePort';
import { HOST_A, HOST_B, host, loadFixtureSnapshot, snapshot, space } from '../support/snapshots';

async function mount(port: MemoryHostsPort) {
  const view = render(<App terminalPort={new MemoryTerminalPort()} layoutPort={new MemoryLayoutPort()} port={port} clientStatePort={new MemoryClientStatePort()} />);
  await screen.findByRole('navigation', { name: 'Spaces' });
  return view;
}

const connected = { kind: 'connected' } as const;

describe('SpaceRail', () => {
  it('marks read-only spaces', async () => {
    const hosts = snapshot([
      host('a', HOST_A, connected, [space(HOST_A, '/src/alpha'), space(HOST_A, '/src/beta')]),
    ]);
    const view = render(
      <SpaceRail
        snapshot={hosts}
        selected={null}
        port={new MemoryHostsPort(hosts)}
        readOnlySpaces={new Set([spaceKeyId({ hostId: HOST_A, path: '/src/beta' })])}
        onSelect={() => undefined}
      />,
    );
    const rail = within(view.container);
    expect(rail.getByRole('button', { name: /^beta/ }).getAttribute('data-read-only')).toBe('true');
    expect(rail.getByRole('button', { name: /^beta/ }).textContent).toContain('read-only');
    expect(rail.getByRole('button', { name: 'alpha' }).hasAttribute('data-read-only')).toBe(false);
  });

  it('shows an invalid hosts.json with its fix hint', async () => {
    const error = 'hosts.json has version 2; this Combine reads version 1';
    await mount(new MemoryHostsPort(snapshot([host('a', HOST_A, connected)], '1', error)));
    expect(screen.getByRole('alert').textContent).toBe(error);
    const hint = document.querySelector('.config-hint');
    expect(hint?.textContent).toContain('/cfg/hosts.json');
    expect(hint?.textContent).toContain('Combine has not changed the file.');
    expect(document.querySelectorAll('section[data-connection]')).toHaveLength(0);
  });

  it('shows where to add hosts when there are none', async () => {
    await mount(new MemoryHostsPort(snapshot([])));
    expect(screen.getByText('No hosts yet. Add them to /cfg/hosts.json.')).toBeTruthy();
  });

  it("shows a failed host's version mismatch message with Retry", async () => {
    const message = 'protocol version mismatch: client 1, daemon 2';
    const port = new MemoryHostsPort(
      snapshot([host('mini', null, { kind: 'failed', message, detail: null })]),
    );
    await mount(port);
    const section = screen.getByRole('region', { name: 'mini' });
    expect(within(section).getByRole('status').textContent).toBe(message);
    expect(section.querySelector('.detail')).toBeNull();
    fireEvent.click(within(section).getByRole('button', { name: 'Retry' }));
    expect(port.retryCalls).toEqual(['mini']);
  });

  it("shows a failed host's SSH failure message and detail", async () => {
    const fixture = loadFixtureSnapshot();
    const work = fixture.hosts.find((h) => h.name === 'work-mbp');
    if (work === undefined || work.connection.kind !== 'failed') throw new Error('fixture');
    await mount(new MemoryHostsPort(fixture));
    const section = screen.getByRole('region', { name: 'work-mbp' });
    expect(within(section).getByRole('status').textContent).toBe(work.connection.message);
    expect(section.querySelector('.detail')?.textContent).toBe(
      'example@work-mbp.local: Permission denied (publickey).',
    );
    expect(within(section).getByRole('button', { name: 'Retry' })).toBeTruthy();
  });

  it('shows Retry only for failed hosts', async () => {
    await mount(new MemoryHostsPort(loadFixtureSnapshot()));
    const retries = screen.getAllByRole('button', { name: 'Retry' });
    expect(retries).toHaveLength(1);
    expect(retries[0]?.closest('section')?.getAttribute('aria-label')).toBe('work-mbp');
  });

  it('renders one section per host in hosts.json order', async () => {
    await mount(
      new MemoryHostsPort(
        snapshot([host('zeta', null, { kind: 'connecting' }), host('alpha', HOST_A, connected), host('mid', null, { kind: 'connecting' })]),
      ),
    );
    const names = [...document.querySelectorAll('nav > section')].map((s) => s.getAttribute('aria-label'));
    expect(names).toEqual(['zeta', 'alpha', 'mid']);
  });

  it('lists spaces by folder name', async () => {
    await mount(
      new MemoryHostsPort(
        snapshot([
          host('a', HOST_A, connected, [
            space(HOST_A, '/src/Zed'),
            space(HOST_A, '/src/alpha'),
            space(HOST_A, '/src/Beta'),
          ]),
        ]),
      ),
    );
    const buttons = within(screen.getByRole('region', { name: 'a' })).getAllByRole('button');
    expect(buttons.map((b) => b.textContent)).toEqual(['alpha', 'Beta', 'Zed']);
    expect(buttons[0]?.getAttribute('title')).toBe('/src/alpha');
  });

  it('shows No spaces for a connected host without spaces', async () => {
    await mount(new MemoryHostsPort(snapshot([host('a', HOST_A, connected)])));
    expect(screen.getByText('No spaces')).toBeTruthy();
  });

  it('reads state through the injected port', async () => {
    const port = new MemoryHostsPort(snapshot([host('first', null, { kind: 'connecting' })]));
    await mount(port);
    expect(screen.getByRole('region', { name: 'first' })).toBeTruthy();
    act(() => port.push(snapshot([host('second', null, { kind: 'connecting' })], '2')));
    expect(screen.getByRole('region', { name: 'second' })).toBeTruthy();
    expect(screen.queryByRole('region', { name: 'first' })).toBeNull();
  });

  it("dims a dropped host's spaces and restores them after reconnect", async () => {
    const detail = 'ssh: connect to host mini port 22: Operation timed out';
    const port = new MemoryHostsPort(
      snapshot([host('mini', HOST_B, connected, [space(HOST_B, '/src/mimir')])]),
    );
    await mount(port);
    const dims = () => {
      const section = screen.getByRole('region', { name: 'mini' });
      return [
        section.getAttribute('data-dimmed'),
        within(section).getByRole('button', { name: 'mimir' }).getAttribute('data-dimmed'),
      ];
    };
    expect(dims()).toEqual(['false', 'false']);
    act(() =>
      port.setConnection('mini', { kind: 'dropped', reason: 'connection closed', detail, retryInMs: 4000 }),
    );
    expect(dims()).toEqual(['true', 'true']);
    expect(document.querySelector('.detail')?.textContent).toBe(detail);
    act(() => port.setConnection('mini', { kind: 'reconnecting', attempt: 1 }));
    expect(dims()).toEqual(['true', 'true']);
    act(() => port.setConnection('mini', connected));
    expect(dims()).toEqual(['false', 'false']);
  });

  it('selection is keyed by host ID and path', async () => {
    const mk = (nameB: string, revision: string) =>
      snapshot(
        [
          host('one', HOST_A, connected, [space(HOST_A, '/src/app')]),
          host(nameB, HOST_B, connected, [space(HOST_B, '/src/app')]),
        ],
        revision,
      );
    const port = new MemoryHostsPort(mk('two', '1'));
    await mount(port);
    const button = (name: string) =>
      within(screen.getByRole('region', { name })).getByRole('button', { name: 'app' });
    fireEvent.click(button('two'));
    expect(button('one').getAttribute('aria-current')).toBeNull();
    expect(button('two').getAttribute('aria-current')).toBe('true');
    act(() => port.push(mk('renamed', '2')));
    await waitFor(() => expect(button('renamed').getAttribute('aria-current')).toBe('true'));
    expect(button('one').getAttribute('aria-current')).toBeNull();
  });
});
