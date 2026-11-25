import { describe, expect, it } from 'vitest';
import type { DecimalU64 } from '../../src/model/ids';
import type { PanelContent, PanelEntry, PanelState } from '../../src/model/panels';
import type { RenderOutcome } from '../../src/model/render';
import {
  type Effect,
  type Input,
  type MachineState,
  decide,
  initialState,
  viewState,
  viewText,
} from '../../src/panels/viewPanelMachine';

const rev = (n: string) => n as DecimalU64;

function listed(revision: string, state: PanelState = 'validated', error: PanelEntryError = null): PanelEntry {
  return {
    kind: 'listed',
    summary: { id: 'p', title: null, renderer: 'markdown', revision: rev(revision), state, error, createdBy: null, lifetime: null },
  };
}
type PanelEntryError = Extract<PanelEntry, { kind: 'listed' }>['summary']['error'];

function content(revision: string, state: PanelState = 'validated'): PanelContent {
  return { space: '/w', id: 'p', revision: rev(revision), renderer: 'markdown', source: `# ${revision}`, state, error: null };
}

const okOutcome = (): RenderOutcome => ({ ok: true, fragment: document.createDocumentFragment() });
const failOutcome: RenderOutcome = { ok: false, error: { message: 'bad', line: 2, column: 5 } };

class Run {
  state: MachineState = initialState();
  effects: Effect[] = [];
  session = 1;
  connected = true;
  focused = false;

  send(input: Input): Effect[] {
    const out = decide(this.state, input);
    this.state = out.state;
    this.effects.push(...out.effects);
    return out.effects;
  }

  entry(entry: PanelEntry): Effect[] {
    return this.send({ kind: 'sync', entry, session: this.session, connected: this.connected, focused: this.focused });
  }

  /** Lists `revision`, answers its fetch and finishes its render. */
  show(revision: string, outcome: RenderOutcome = okOutcome()): void {
    const [fetch] = this.entry(listed(revision));
    if (fetch?.kind !== 'fetch') throw new Error('expected a fetch');
    const [render] = this.send({ kind: 'fetched', token: fetch.token, content: content(revision) });
    if (render?.kind !== 'render') throw new Error('expected a render');
    this.send({ kind: 'rendered', token: render.token, outcome });
  }

  kinds(): string[] {
    return this.effects.map((e) => e.kind);
  }
}

describe('viewPanelMachine fetching', () => {
  it('F1 fetches a connected validated revision, urgent when focused', () => {
    const run = new Run();
    run.focused = true;
    expect(run.entry(listed('5'))).toEqual([{ kind: 'fetch', token: 1, revision: '5', urgent: true }]);
    expect(run.state.fetching).toEqual({ token: 1, revision: '5', session: 1 });
  });

  it('F1 fetches validated_parser_only and nothing else', () => {
    const run = new Run();
    expect(run.entry(listed('5', 'validated_parser_only'))).toHaveLength(1);
    for (const state of ['validating', 'fetching', 'failed', 'untrusted'] as const) {
      expect(new Run().entry(listed('5', state))).toEqual([]);
    }
  });

  it('F1 waits for a connection and for a newer revision', () => {
    const run = new Run();
    run.connected = false;
    expect(run.entry(listed('5'))).toEqual([]);
    run.connected = true;
    expect(run.entry(listed('5'))).toHaveLength(1);
    expect(run.entry(listed('5'))).toEqual([]);
    expect(run.entry(listed('4'))).toEqual([]);
    expect(run.entry(listed('6'))).toEqual([{ kind: 'fetch', token: 2, revision: '6', urgent: false }]);
  });

  it('F1 compares revisions as u64, not as numbers', () => {
    const run = new Run();
    run.show('9007199254740992');
    expect(run.entry(listed('9007199254740993'))).toHaveLength(1);
    expect(run.entry(listed('9007199254740992'))).toEqual([]);
  });

  it('F2 renders what a current fetch returned, and accepts a revision newer than the list', () => {
    const run = new Run();
    run.entry(listed('5'));
    expect(run.send({ kind: 'fetched', token: 1, content: content('7') })).toEqual([
      { kind: 'render', token: 2, renderer: 'markdown', source: '# 7' },
    ]);
    expect(run.state.latestAccepted).toBe('7');
    expect(run.state.rendering).toEqual({ token: 2, revision: '7' });
  });

  it('F2 refuses a reply that is not validated or not newer', () => {
    const run = new Run();
    run.entry(listed('5'));
    expect(run.send({ kind: 'fetched', token: 1, content: content('5', 'failed') })).toEqual([]);
    expect(run.state.latestAccepted).toBeNull();
    expect(run.state.fetching).toBeNull();
    run.show('8');
    run.entry(listed('9'));
    expect(run.send({ kind: 'fetched', token: 4, content: content('8') })).toEqual([]);
  });

  it('F3 ignores a reply or failure with a stale token', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.entry(listed('6'));
    const before = run.state;
    expect(run.send({ kind: 'fetched', token: 1, content: content('5') })).toEqual([]);
    expect(run.send({ kind: 'fetchFailed', token: 1, message: 'superseded' })).toEqual([]);
    expect(run.state).toBe(before);
  });

  it('F4 remembers a failure and does not retry it in the same session', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.send({ kind: 'fetchFailed', token: 1, message: 'boom' });
    expect(run.state.fetchFailed).toEqual({ revision: '5', session: 1, message: 'boom' });
    expect(run.entry(listed('5'))).toEqual([]);
    expect(run.entry(listed('6'))).toHaveLength(1);
  });

  it('F4 retries after a reconnect', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.send({ kind: 'fetchFailed', token: 1, message: 'The host disconnected.' });
    run.session = 2;
    expect(run.entry(listed('5'))).toHaveLength(1);
  });

  it('a_failure_arriving_after_the_session_bump_is_retried_after_the_reconnect', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.session = 2;
    run.connected = false;
    run.entry(listed('5'));
    run.send({ kind: 'fetchFailed', token: 1, message: 'The host disconnected.' });
    expect(run.state.fetchFailed).toEqual({ revision: '5', session: 1, message: 'The host disconnected.' });
    run.connected = true;
    expect(run.entry(listed('5'))).toEqual([{ kind: 'fetch', token: 2, revision: '5', urgent: false }]);
  });

  it('a_fetch_still_pending_at_the_reconnect_is_abandoned_and_fetched_again', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.session = 2;
    run.connected = false;
    run.entry(listed('5'));
    run.connected = true;
    expect(run.entry(listed('5'))).toEqual([{ kind: 'fetch', token: 2, revision: '5', urgent: false }]);
    expect(run.entry(listed('5'))).toEqual([]);
    expect(run.send({ kind: 'fetched', token: 1, content: content('5', 'validated') })).toEqual([]);
    expect(run.send({ kind: 'fetchFailed', token: 1, message: 'late' })).toEqual([]);
    expect(run.state.fetchFailed).toBeNull();
    expect(run.send({ kind: 'fetched', token: 2, content: content('5') }).map((e) => e.kind)).toEqual(['render']);
  });

  it('a_stale_reply_is_ignored_by_the_token_check_itself', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.entry(listed('6'));
    // Token 1 is stale although its revision is still acceptable: only the token check refuses it.
    expect(run.send({ kind: 'fetched', token: 1, content: content('7') })).toEqual([]);
    expect(run.state.latestAccepted).toBeNull();
    expect(run.state.fetching).toEqual({ token: 2, revision: '6', session: 1 });
  });

  it('F5 shows the render and F6 discards a stale one', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.send({ kind: 'fetched', token: 1, content: content('5') });
    run.entry(listed('6'));
    run.send({ kind: 'fetched', token: 3, content: content('6') });
    expect(run.send({ kind: 'rendered', token: 2, outcome: okOutcome() })).toEqual([]);
    expect(run.state.shown).toBeNull();
    const effects = run.send({ kind: 'rendered', token: 4, outcome: okOutcome() });
    expect(effects.map((e) => e.kind)).toEqual(['show', 'report']);
    expect(run.state.shown).toEqual({ revision: '6', status: 'rendered', error: null });
  });

  it('F5 shows a render failure as an error and clears the content', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.send({ kind: 'fetched', token: 1, content: content('5') });
    const effects = run.send({ kind: 'rendered', token: 2, outcome: failOutcome });
    expect(effects[0]).toEqual({ kind: 'show', fragment: null });
    expect(run.state.shown).toEqual({
      revision: '5',
      status: 'render_failed',
      error: { code: 'render_error', message: 'bad', line: 2, column: 5, snippet: null },
    });
  });

  it('F7 resets on missing or skipped and clears the content', () => {
    for (const entry of [{ kind: 'missing' }, { kind: 'skipped', reason: 'r' }] as PanelEntry[]) {
      const run = new Run();
      run.show('5');
      expect(run.entry(entry)).toEqual([{ kind: 'clear' }]);
      expect(run.state).toEqual({ ...initialState(), nextToken: run.state.nextToken, session: 1, connected: true });
      run.effects = [];
      expect(run.entry(listed('5'))).toHaveLength(1);
    }
  });

  it('F8 does nothing for an out-of-order list', () => {
    const run = new Run();
    run.show('8');
    const before = run.state;
    expect(run.entry(listed('7'))).toEqual([]);
    expect(run.state).toEqual(before);
    expect(run.state.shown?.revision).toBe('8');
  });

  it('F9 keeps what is shown and sends no fetch or report for a revision that is not ready', () => {
    const run = new Run();
    run.show('5');
    run.effects = [];
    for (const state of ['validating', 'fetching', 'untrusted', 'failed'] as const) {
      expect(run.entry(listed('6', state))).toEqual([]);
    }
    expect(run.state.shown?.revision).toBe('5');
  });

  it('F11 abandons everything on unmount and then ignores every input', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.send({ kind: 'unmount' });
    expect(run.send({ kind: 'fetched', token: 1, content: content('5') })).toEqual([]);
    expect(run.entry(listed('6'))).toEqual([]);
    expect(run.state.closed).toBe(true);
  });
});

describe('viewPanelMachine reports', () => {
  it('R1 reports a shown revision once per session', () => {
    const run = new Run();
    run.show('5');
    expect(run.effects.filter((e) => e.kind === 'report')).toEqual([
      { kind: 'report', shown: { revision: '5', status: 'rendered', error: null } },
    ]);
    run.send({ kind: 'reportDone', sent: true });
    expect(run.state.reportedSession).toBe(1);
    expect(run.entry(listed('5'))).toEqual([]);
  });

  it('R1 waits while a report is in flight', () => {
    const run = new Run();
    run.show('5');
    expect(run.state.reportInFlight).toEqual({ revision: '5', session: 1 });
    expect(run.entry(listed('5'))).toEqual([]);
  });

  it('R2 reports a newer shown revision when the older report returns', () => {
    const run = new Run();
    run.show('5');
    run.show('6');
    expect(run.kinds().filter((k) => k === 'report')).toHaveLength(1);
    const effects = run.send({ kind: 'reportDone', sent: true });
    expect(effects).toEqual([{ kind: 'report', shown: { revision: '6', status: 'rendered', error: null } }]);
    expect(run.state.reportedSession).toBeNull();
  });

  it('a_render_done_while_disconnected_is_reported_once_after_the_reconnect', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.connected = false;
    run.entry(listed('5'));
    run.send({ kind: 'fetched', token: 1, content: content('5') });
    run.send({ kind: 'rendered', token: 2, outcome: okOutcome() });
    expect(run.kinds()).not.toContain('report');
    expect(run.state.reportedSession).toBeNull();
    run.session = 2;
    run.connected = true;
    expect(run.entry(listed('5')).map((e) => e.kind)).toEqual(['report']);
    run.send({ kind: 'reportDone', sent: true });
    expect(run.entry(listed('5'))).toEqual([]);
    expect(run.kinds().filter((k) => k === 'report')).toHaveLength(1);
  });

  it('a_report_not_accepted_is_sent_once_after_the_reconnect', () => {
    const run = new Run();
    run.show('5');
    run.send({ kind: 'reportDone', sent: false });
    expect(run.state.reportBlockedSession).toBe(1);
    expect(run.entry(listed('5'))).toEqual([]);
    run.session = 2;
    expect(run.entry(listed('5')).map((e) => e.kind)).toEqual(['report']);
    run.send({ kind: 'reportDone', sent: false });
    expect(run.entry(listed('5'))).toEqual([]);
    expect(run.kinds().filter((k) => k === 'report')).toHaveLength(2);
  });

  it('a report sent just before a drop is sent again after the reconnect', () => {
    const run = new Run();
    run.show('5');
    run.session = 2;
    run.connected = false;
    run.entry(listed('5'));
    run.send({ kind: 'reportDone', sent: true });
    expect(run.state.reportedSession).toBe(1);
    run.connected = true;
    expect(run.entry(listed('5')).map((e) => e.kind)).toEqual(['report']);
  });

  it('a_report_result_with_no_report_in_flight_is_ignored', () => {
    const run = new Run();
    run.show('5');
    run.entry({ kind: 'missing' });
    const reset = run.state;
    run.effects = [];
    expect(run.send({ kind: 'reportDone', sent: true })).toEqual([]);
    expect(run.state).toBe(reset);

    const other = new Run();
    other.show('5');
    other.send({ kind: 'unmount' });
    const closed = other.state;
    expect(other.send({ kind: 'reportDone', sent: true })).toEqual([]);
    expect(other.state).toBe(closed);
  });

  it('never reports a revision that was not shown', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.send({ kind: 'fetchFailed', token: 1, message: 'x' });
    run.entry(listed('6'));
    run.entry(listed('7'));
    run.send({ kind: 'fetched', token: 3, content: content('7') });
    run.send({ kind: 'fetched', token: 2, content: content('6') });
    expect(run.kinds()).not.toContain('report');
  });
});

describe('viewState and viewText', () => {
  const text = (run: Run, entry: PanelEntry) => viewText(run.state, entry, 'p');

  it('missing, skipped and loading', () => {
    const run = new Run();
    expect(viewState(run.state, { kind: 'missing' })).toBe('missing');
    expect(text(run, { kind: 'missing' }).status).toBe('Panel p is missing');
    expect(text(run, { kind: 'skipped', reason: 'too big' }).status).toBe("Panel p isn't loaded: too big");
    expect(text(run, { kind: 'loading' }).status).toBe('Loading…');
  });

  it('fetching and rendering say Loading… only while nothing is shown', () => {
    const run = new Run();
    run.entry(listed('5'));
    expect(viewState(run.state, listed('5'))).toBe('fetching');
    expect(text(run, listed('5')).status).toBe('Loading…');
    run.send({ kind: 'fetched', token: 1, content: content('5') });
    expect(viewState(run.state, listed('5'))).toBe('rendering');
    run.send({ kind: 'rendered', token: 2, outcome: okOutcome() });
    run.entry(listed('6'));
    expect(viewState(run.state, listed('6'))).toBe('fetching');
    expect(text(run, listed('6')).status).toBeNull();
  });

  it('a newer revision that is not ready shows its status over the shown content', () => {
    const run = new Run();
    run.show('5');
    expect(viewState(run.state, listed('6', 'validating'))).toBe('validating');
    expect(text(run, listed('6', 'untrusted')).status).toBe('Validating…');
    const failed = listed('6', 'failed', { code: 'syntax', message: 'unexpected token', line: 2, column: 11, snippet: '  A -> B' });
    expect(viewState(run.state, failed)).toBe('failed');
    expect(text(run, failed)).toEqual({
      status: null,
      error: 'Validation failed at line 2, column 11: unexpected token',
      snippet: '  A -> B',
    });
  });

  it('validation failure copy with line only and with no position', () => {
    const run = new Run();
    const lineOnly = listed('1', 'failed', { code: 'c', message: 'm', line: 3, column: null, snippet: null });
    expect(text(run, lineOnly).error).toBe('Validation failed at line 3: m');
    const none = listed('1', 'failed', { code: 'c', message: 'm', line: null, column: null, snippet: null });
    expect(text(run, none).error).toBe('Validation failed: m');
    expect(text(run, none).snippet).toBeNull();
  });

  it('render failure copy', () => {
    const run = new Run();
    run.show('5', failOutcome);
    expect(viewState(run.state, listed('5'))).toBe('render_failed');
    expect(text(run, listed('5')).error).toBe("Couldn't draw this panel at line 2, column 5: bad");
    const lineOnly = new Run();
    lineOnly.show('5', { ok: false, error: { message: 'bad', line: 4 } });
    expect(text(lineOnly, listed('5')).error).toBe("Couldn't draw this panel at line 4: bad");
    const bare = new Run();
    bare.show('5', { ok: false, error: { message: 'bad' } });
    expect(text(bare, listed('5')).error).toBe("Couldn't draw this panel: bad");
  });

  it('fetch failure copy, rendered, and the precedence', () => {
    const run = new Run();
    run.entry(listed('5'));
    run.send({ kind: 'fetchFailed', token: 1, message: 'No reply' });
    expect(viewState(run.state, listed('5'))).toBe('fetch_failed');
    expect(text(run, listed('5')).error).toBe("Couldn't load panel p: No reply");
    expect(viewState(run.state, listed('5', 'validating'))).toBe('validating');
    const ok = new Run();
    ok.show('5');
    expect(viewState(ok.state, listed('5'))).toBe('rendered');
    expect(text(ok, listed('5'))).toEqual({ status: null, error: null, snippet: null });
    expect(viewState(initialState(), listed('5', 'validating'))).toBe('validating');
    expect(viewState(initialState(), listed('5'))).toBe('loading');
  });

  it('a_failed_refresh_is_shown_beside_the_older_revision_until_a_newer_one_renders', () => {
    const run = new Run();
    run.show('1');
    run.entry(listed('2'));
    run.send({ kind: 'fetchFailed', token: run.state.fetching?.token ?? -1, message: 'No reply' });
    expect(run.state.shown?.revision).toBe('1');
    expect(viewState(run.state, listed('2'))).toBe('fetch_failed');
    expect(text(run, listed('2')).error).toBe("Couldn't load panel p: No reply");
    expect(run.entry(listed('2'))).toEqual([]);

    const [fetch] = run.entry(listed('3'));
    expect(fetch).toMatchObject({ kind: 'fetch', revision: '3' });
    const token = run.state.fetching?.token ?? -1;
    run.send({ kind: 'fetched', token, content: content('3') });
    expect(run.state.fetchFailed).toBeNull();
    run.send({ kind: 'rendered', token: run.state.rendering?.token ?? -1, outcome: okOutcome() });
    expect(viewState(run.state, listed('3'))).toBe('rendered');
    expect(text(run, listed('3'))).toEqual({ status: null, error: null, snippet: null });
  });
});
