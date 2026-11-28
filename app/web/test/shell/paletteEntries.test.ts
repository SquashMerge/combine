import { describe, expect, it } from 'vitest';
import { plainText } from '../../src/model/markedText';
import type { PanelState, PanelSummary } from '../../src/model/panels';
import {
  type PalettePanels,
  buildPaletteEntries,
  runPaletteAction,
} from '../../src/shell/paletteEntries';
import { HOST_A, HOST_B, host, snapshot, space, terminal } from '../support/snapshots';

const connected = { kind: 'connected' } as const;
const failed = { kind: 'failed', message: 'boom', detail: null } as const;

function samplePanels(): PalettePanels {
  const summary: PanelSummary = {
    id: 'notes',
    title: 'Notes',
    renderer: 'markdown',
    revision: '1' as PanelSummary['revision'],
    state: 'validated',
    error: null,
    createdBy: null,
    lifetime: 'persistent',
  };
  return {
    space: { hostId: HOST_A, path: '/src/alpha' },
    hostName: 'mac',
    panels: [summary],
    tiles: new Map([['notes', 'tile-1']]),
    focused: { tile: 'tile-1', panel: 'notes' },
    editable: true,
  };
}

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

  it('every entry marks its detail and the marks flatten to detail', () => {
    for (const entry of buildPaletteEntries(sample(), samplePanels())) {
      expect(entry.detailMarked).toBeDefined();
      expect(plainText(entry.detailMarked!)).toBe(entry.detail);
    }
  });

  it('marks the paths, folders and commands in each detail', () => {
    const codes = (kind: string) =>
      buildPaletteEntries(sample(), samplePanels())
        .filter((entry) => entry.action.kind === kind)
        .map((entry) =>
          Array.isArray(entry.detailMarked)
            ? entry.detailMarked.flatMap((part) => (typeof part === 'string' ? [] : [part.code]))
            : [],
        );
    expect(codes('switch-space')).toEqual([['/src/alpha'], ['/src/mimir']]);
    expect(codes('focus-terminal')).toEqual([
      ['alpha', 'first'],
      ['mimir', 'remote'],
    ]);
    expect(codes('show-panel')).toEqual([['alpha']]);
    expect(codes('close-panel')).toEqual([['alpha']]);
    expect(codes('show-active-terminal')).toEqual([['alpha']]);
    expect(codes('retry-host')).toEqual([[]]);
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

  describe('panels', () => {
    const alpha = { hostId: HOST_A, path: '/src/alpha' };

    function summary(
      id: string,
      title: string | null,
      state: PanelState = 'validated',
      lifetime: PanelSummary['lifetime'] = 'persistent',
    ): PanelSummary {
      return {
        id,
        title,
        renderer: 'markdown',
        revision: '1' as PanelSummary['revision'],
        state,
        error: null,
        createdBy: null,
        lifetime,
      };
    }

    function panels(over: Partial<PalettePanels> = {}): PalettePanels {
      return {
        space: alpha,
        hostName: 'mac',
        panels: [summary('notes', 'Notes'), summary('plain', null, 'validated'), summary('wip', 'Wip', 'validated_parser_only', 'scratch')],
        tiles: new Map([
          ['notes', 'tile-1'],
          ['wip', 'tile-1'],
        ]),
        focused: { tile: 'tile-1', panel: 'wip' },
        editable: true,
        ...over,
      };
    }

    const panelEntries = (p: PalettePanels) =>
      buildPaletteEntries(sample(), p).filter((e) => e.action.kind === 'show-panel');

    it("lists the watched space's panels with title or ID and a detail naming space, renderer and state", () => {
      const entries = panelEntries(panels());
      expect(entries.map((e) => e.label)).toEqual(['Notes', 'plain', 'Wip']);
      expect(entries[0]?.detail).toBe('mac alpha markdown validated');
      expect(entries[2]?.detail).toBe('mac alpha markdown validated parser only');
      expect(entries[0]?.action).toEqual({ kind: 'show-panel', space: alpha, panel: 'notes', tile: 'tile-1' });
    });

    it('marks a panel in no tile', () => {
      const [, plain] = panelEntries(panels());
      expect(plain?.detail).toBe('mac alpha markdown validated, not in a tile');
      expect(plain?.action).toEqual({ kind: 'show-panel', space: alpha, panel: 'plain', tile: null });
    });

    it('omits removed panels', () => {
      const entries = panelEntries(panels({ panels: [summary('gone', 'Gone', 'removed'), summary('notes', 'Notes')] }));
      expect(entries.map((e) => e.label)).toEqual(['Notes']);
    });

    it('puts panel actions first and panels before hosts', () => {
      const entries = buildPaletteEntries(sample(), panels());
      const kinds = entries.map((e) => e.action.kind);
      expect(kinds).toEqual([
        'close-panel',
        'pin-panel',
        'show-active-terminal',
        'switch-space',
        'switch-space',
        'focus-terminal',
        'focus-terminal',
        'show-panel',
        'show-panel',
        'show-panel',
        'retry-host',
      ]);
      expect(entries[0]).toMatchObject({
        label: 'Close panel',
        detail: 'Wip in alpha',
        action: { kind: 'close-panel', space: alpha, panel: 'wip' },
      });
      expect(entries[1]).toMatchObject({ label: 'Pin panel', action: { kind: 'pin-panel', panel: 'wip' } });
    });

    it('offers Pin panel only for a scratch panel', () => {
      const kinds = buildPaletteEntries(sample(), panels({ focused: { tile: 'tile-1', panel: 'notes' } })).map(
        (e) => e.action.kind,
      );
      expect(kinds).toContain('close-panel');
      expect(kinds).not.toContain('pin-panel');
    });

    it('offers Close panel for a focused tab missing from the list', () => {
      const entries = buildPaletteEntries(sample(), panels({ focused: { tile: 'tile-1', panel: 'ghost' } }));
      expect(entries[0]).toMatchObject({ label: 'Close panel', detail: 'ghost in alpha' });
    });

    it('a held layout offers no close, pin or place entries', () => {
      const entries = buildPaletteEntries(sample(), panels({ editable: false }));
      const kinds = entries.map((e) => e.action.kind);
      expect(kinds).not.toContain('close-panel');
      expect(kinds).not.toContain('pin-panel');
      expect(entries.filter((e) => e.action.kind === 'show-panel').map((e) => e.label)).toEqual(['Notes', 'Wip']);
    });

    it('show_the_active_terminal_here', () => {
      const find = (p: PalettePanels) =>
        buildPaletteEntries(sample(), p).find((e) => e.action.kind === 'show-active-terminal');
      expect(find(panels())).toMatchObject({
        label: 'Show the active terminal here',
        detail: 'in alpha',
        action: { kind: 'show-active-terminal', space: alpha, tile: 'tile-1' },
      });
      expect(find(panels({ focused: { tile: 'tile-1', panel: '.active-terminal' } }))).toBeUndefined();
      expect(find(panels({ editable: false }))).toBeUndefined();
      expect(find(panels({ focused: null, focusedTile: 'tile-2' }))?.action).toMatchObject({ tile: 'tile-2' });
    });

    it('names a focused terminal tab by its label in the Close panel detail', () => {
      const entries = buildPaletteEntries(
        sample(),
        panels({ focused: { tile: 'tile-1', panel: '.active-terminal', label: 'Active: build' } }),
      );
      expect(entries[0]).toMatchObject({ label: 'Close panel', detail: 'Active: build in alpha' });
    });

    it('runs the show-active-terminal handler', () => {
      const calls: string[] = [];
      const entry = buildPaletteEntries(sample(), panels()).find((e) => e.action.kind === 'show-active-terminal');
      const base = { onSelectSpace: () => {}, onSelectTerminal: () => {}, onRetryHost: () => {} };
      if (entry === undefined) throw new Error('no entry');
      runPaletteAction(entry.action, { ...base, onShowActiveTerminal: (_s, tile) => calls.push(tile) });
      runPaletteAction(entry.action, base);
      expect(calls).toEqual(['tile-1']);
    });

    it('lists what it listed before with no panels argument', () => {
      const kinds = buildPaletteEntries(sample()).map((e) => e.action.kind);
      expect(kinds).toEqual(['switch-space', 'switch-space', 'focus-terminal', 'focus-terminal', 'retry-host']);
    });

    it('runs the panel handlers, and a missing handler is a no-op', () => {
      const calls: string[] = [];
      const handlers = {
        onSelectSpace: () => {},
        onSelectTerminal: () => {},
        onRetryHost: () => {},
        onShowPanel: (_s: unknown, panel: string, tile: string | null) => calls.push(`show ${panel} ${tile}`),
        onClosePanel: (_s: unknown, panel: string) => calls.push(`close ${panel}`),
        onPinPanel: (_s: unknown, panel: string) => calls.push(`pin ${panel}`),
      };
      for (const entry of buildPaletteEntries(sample(), panels())) {
        if (['show-panel', 'close-panel', 'pin-panel'].includes(entry.action.kind)) {
          runPaletteAction(entry.action, handlers);
          runPaletteAction(entry.action, { ...handlers, onShowPanel: undefined, onClosePanel: undefined, onPinPanel: undefined });
        }
      }
      expect(calls).toEqual(['close wip', 'pin wip', 'show notes tile-1', 'show plain null', 'show wip tile-1']);
    });
  });
});
