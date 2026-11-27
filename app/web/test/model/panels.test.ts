import { describe, expect, it } from 'vitest';
import type { DecimalU64 } from '../../src/model/ids';
import {
  type PanelList,
  type PanelSummary,
  PANELS_LOADING,
  isLoadingList,
  panelEntry,
  parsePanelsChanged,
  tabLabel,
} from '../../src/model/panels';
import { loadPanelsEventFixture } from '../support/memoryPanelPort';

const fixture = loadPanelsEventFixture();

function summary(overrides: Partial<PanelSummary> = {}): PanelSummary {
  return {
    id: 'notes',
    title: null,
    renderer: 'markdown',
    revision: '1' as DecimalU64,
    state: 'validated',
    error: null,
    createdBy: null,
    lifetime: 'persistent',
    ...overrides,
  };
}

function list(panels: PanelSummary[], skipped: PanelList['skipped'] = []): PanelList {
  return { space: '/work', panels, skipped, error: null };
}

describe('parsePanelsChanged', () => {
  it('parses_every_kind_of_the_shared_fixture', () => {
    expect(Object.keys(fixture)).toEqual(['list', 'push', 'loading', 'panel', 'error', 'closed', 'pinned', 'disconnected']);
    const listed = parsePanelsChanged(fixture.list);
    expect(listed).toMatchObject({ kind: 'list', hostId: 'host-1', request: '7' });
    if (listed.kind !== 'list') throw new Error('a list');
    const [classes, notes] = listed.list.panels;
    expect(classes).toEqual({
      id: 'classes',
      title: 'Class diagram',
      renderer: 'mermaid',
      revision: '9007199254740993',
      state: 'failed',
      error: { code: 'syntax', message: 'unexpected token', line: 2, column: 11, snippet: '  A -> B' },
      createdBy: { type: 'agent', slot: 'claude' },
      lifetime: 'persistent',
    });
    expect(notes).toMatchObject({ title: null, state: 'validated', error: null, createdBy: { type: 'user' } });
    expect(listed.list.skipped).toEqual([{ panel: 'broken', reason: "panel.json can't be used" }]);
    expect(parsePanelsChanged(fixture.push)).toMatchObject({ kind: 'list', request: null });
    const loading = parsePanelsChanged(fixture.loading);
    if (loading.kind !== 'list') throw new Error('a list');
    expect(isLoadingList(loading.list)).toBe(true);
    expect(loading.list.error).toBe(PANELS_LOADING);
    expect(parsePanelsChanged(fixture.panel)).toMatchObject({
      kind: 'panel',
      request: '9',
      panel: { id: 'notes', revision: '12', source: '# Notes\n', state: 'validated' },
    });
    expect(parsePanelsChanged(fixture.error)).toEqual({
      kind: 'error',
      hostId: 'host-1',
      request: '10',
      code: 'unknown_panel',
      message: 'the space has no panel gone',
    });
    expect(parsePanelsChanged(fixture.disconnected)).toEqual({ kind: 'disconnected', hostId: 'host-1' });
  });

  it('parses lifetime, closed and pinned', () => {
    const listed = parsePanelsChanged(fixture.list);
    if (listed.kind !== 'list') throw new Error('a list');
    expect(listed.list.panels.map((p) => p.lifetime)).toEqual(['persistent', 'scratch']);
    expect(parsePanelsChanged(fixture.closed)).toEqual({
      kind: 'closed',
      hostId: 'host-1',
      request: '11',
      space: '/work',
      panel: 'notes',
    });
    expect(parsePanelsChanged(fixture.pinned)).toEqual({
      kind: 'pinned',
      hostId: 'host-1',
      request: '12',
      space: '/work',
      panel: 'notes',
    });
    const entry = (fixture.list['list'] as { panels: Record<string, unknown>[] }).panels[0] ?? {};
    const withLifetime = (lifetime: unknown): unknown => ({
      ...fixture.list,
      list: { ...(fixture.list['list'] as object), panels: [{ ...entry, lifetime }] },
    });
    expect(() => parsePanelsChanged(withLifetime(null))).toThrow('lifetime');
    expect(() => parsePanelsChanged(withLifetime('forever'))).toThrow('lifetime');
    expect(() => parsePanelsChanged(withLifetime(undefined))).toThrow('lifetime');
    expect(() => parsePanelsChanged({ ...fixture.closed, space: 3 })).toThrow('space');
    expect(() => parsePanelsChanged({ ...fixture.pinned, request: 12 })).toThrow();
  });

  it('keeps a revision above 2^53 exact', () => {
    const listed = parsePanelsChanged(fixture.list);
    if (listed.kind !== 'list') throw new Error('a list');
    expect(listed.list.panels[0]?.revision).toBe('9007199254740993');
  });

  it('rejects malformed events', () => {
    expect(() => parsePanelsChanged({ kind: 'nope', hostId: 'h' })).toThrow();
    expect(() => parsePanelsChanged({ ...fixture.panel, panel: { ...(fixture.panel['panel'] as object), state: 'bogus' } })).toThrow();
    expect(() => parsePanelsChanged({ ...fixture.error, request: 10 })).toThrow();
  });
});

describe('panelEntry and tabLabel', () => {
  it('a null list is loading', () => {
    expect(panelEntry(null, 'a')).toEqual({ kind: 'loading' });
    expect(tabLabel(null, 'a')).toBe('a');
  });

  it('a listed panel is listed and shows its title', () => {
    const l = list([summary({ id: 'a', title: 'Alpha' })]);
    expect(panelEntry(l, 'a')).toMatchObject({ kind: 'listed' });
    expect(tabLabel(l, 'a')).toBe('Alpha');
  });

  it('a removed panel and an unknown id are missing', () => {
    const l = list([summary({ id: 'a', state: 'removed', title: 'Gone' })]);
    expect(panelEntry(l, 'a')).toEqual({ kind: 'missing' });
    expect(panelEntry(l, 'b')).toEqual({ kind: 'missing' });
    expect(tabLabel(l, 'a')).toBe('a');
  });

  it('a skipped id gives its reason', () => {
    const l = list([], [{ panel: 'x', reason: 'bad' }]);
    expect(panelEntry(l, 'x')).toEqual({ kind: 'skipped', reason: 'bad' });
  });

  it('a removed entry that is also skipped is skipped', () => {
    const l = list([summary({ id: 'a', state: 'removed' })], [{ panel: 'a', reason: 'r' }]);
    expect(panelEntry(l, 'a')).toEqual({ kind: 'skipped', reason: 'r' });
  });
});
