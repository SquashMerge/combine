import { type DecimalU64, compareDecimalU64 } from '../model/ids';
import type { PanelContent, PanelEntry, PanelError, PanelState, RenderStatus } from '../model/panels';
import type { RenderOutcome, RendererName } from '../model/render';

export interface Shown {
  revision: DecimalU64;
  status: RenderStatus;
  error: PanelError | null;
}

export interface MachineState {
  latestAccepted: DecimalU64 | null;
  /** `session` is the host session the request was issued in. */
  fetching: { token: number; revision: DecimalU64; session: number } | null;
  fetchFailed: { revision: DecimalU64; session: number; message: string } | null;
  rendering: { token: number; revision: DecimalU64 } | null;
  shown: Shown | null;
  /** The host session in which the shown revision's report was accepted. */
  reportedSession: number | null;
  reportInFlight: { revision: DecimalU64; session: number } | null;
  /** The host session in which the host refused the shown revision's report. */
  reportBlockedSession: number | null;
  nextToken: number;
  session: number;
  connected: boolean;
  closed: boolean;
}

export type Input =
  | { kind: 'sync'; entry: PanelEntry; session: number; connected: boolean; focused: boolean }
  | { kind: 'fetched'; token: number; content: PanelContent }
  | { kind: 'fetchFailed'; token: number; message: string }
  | { kind: 'rendered'; token: number; outcome: RenderOutcome }
  | { kind: 'reportDone'; sent: boolean }
  | { kind: 'unmount' };

export type Effect =
  | { kind: 'fetch'; token: number; revision: DecimalU64; urgent: boolean }
  | { kind: 'render'; token: number; renderer: RendererName; source: string }
  | { kind: 'report'; shown: Shown }
  | { kind: 'clear' }
  | { kind: 'show'; fragment: DocumentFragment | null };

export interface Decision {
  state: MachineState;
  effects: Effect[];
}

export const RENDER_ERROR_CODE = 'render_error';

export function initialState(): MachineState {
  return {
    latestAccepted: null,
    fetching: null,
    fetchFailed: null,
    rendering: null,
    shown: null,
    reportedSession: null,
    reportInFlight: null,
    reportBlockedSession: null,
    nextToken: 1,
    session: 0,
    connected: false,
    closed: false,
  };
}

function isNewer(revision: DecimalU64, than: DecimalU64 | null): boolean {
  return than === null || compareDecimalU64(revision, than) > 0;
}

function fetchable(state: PanelState): boolean {
  return state === 'validated' || state === 'validated_parser_only';
}

/** R1: report the shown revision once per host session in which it is shown while connected. */
function report(state: MachineState, effects: Effect[]): MachineState {
  const { shown } = state;
  if (
    shown === null ||
    !state.connected ||
    state.reportedSession === state.session ||
    state.reportInFlight !== null ||
    state.reportBlockedSession === state.session
  ) {
    return state;
  }
  effects.push({ kind: 'report', shown });
  return { ...state, reportInFlight: { revision: shown.revision, session: state.session } };
}

function shownFrom(revision: DecimalU64, outcome: RenderOutcome): Shown {
  if (outcome.ok) return { revision, status: 'rendered', error: null };
  const { message, line, column } = outcome.error;
  return {
    revision,
    status: 'render_failed',
    error: { code: RENDER_ERROR_CODE, message, line: line ?? null, column: column ?? null, snippet: null },
  };
}

/** The ViewPanel state machine of the brief's section 5.2, as a pure function. */
export function decide(state: MachineState, input: Input): Decision {
  const effects: Effect[] = [];
  if (state.closed) return { state, effects };
  switch (input.kind) {
    case 'sync': {
      let next: MachineState = { ...state, session: input.session, connected: input.connected };
      const { entry } = input;
      if (entry.kind === 'missing' || entry.kind === 'skipped') {
        // F7
        next = {
          ...initialState(),
          nextToken: next.nextToken,
          session: next.session,
          connected: next.connected,
        };
        effects.push({ kind: 'clear' });
        return { state: next, effects };
      }
      if (entry.kind === 'listed' && next.connected) {
        const { revision } = entry.summary;
        const failed = next.fetchFailed;
        if (
          fetchable(entry.summary.state) &&
          isNewer(revision, next.latestAccepted) &&
          (next.fetching === null ||
            compareDecimalU64(next.fetching.revision, revision) < 0 ||
            next.fetching.session < next.session) &&
          !(failed !== null && compareDecimalU64(failed.revision, revision) >= 0 && failed.session === next.session)
        ) {
          // F1
          const token = next.nextToken;
          next = { ...next, nextToken: token + 1, fetching: { token, revision, session: next.session } };
          effects.push({ kind: 'fetch', token, revision, urgent: input.focused });
        }
      }
      return { state: report(next, effects), effects };
    }
    case 'fetched': {
      if (state.fetching?.token !== input.token) return { state, effects }; // F3
      let next: MachineState = { ...state, fetching: null };
      const { content } = input;
      if (fetchable(content.state) && isNewer(content.revision, next.latestAccepted)) {
        // F2
        const token = next.nextToken;
        next = {
          ...next,
          latestAccepted: content.revision,
          nextToken: token + 1,
          rendering: { token, revision: content.revision },
        };
        effects.push({ kind: 'render', token, renderer: content.renderer, source: content.source });
      }
      return { state: report(next, effects), effects };
    }
    case 'fetchFailed': {
      const { fetching } = state;
      if (fetching?.token !== input.token) return { state, effects }; // F3
      // F4
      const next: MachineState = {
        ...state,
        fetching: null,
        fetchFailed: { revision: fetching.revision, session: fetching.session, message: input.message },
      };
      return { state: report(next, effects), effects };
    }
    case 'rendered': {
      const { rendering } = state;
      if (rendering?.token !== input.token) return { state, effects }; // F6
      // F5
      const shown = shownFrom(rendering.revision, input.outcome);
      effects.push({ kind: 'show', fragment: input.outcome.ok ? input.outcome.fragment : null });
      const next: MachineState = {
        ...state,
        rendering: null,
        shown,
        reportedSession: null,
        reportBlockedSession: null,
      };
      return { state: report(next, effects), effects };
    }
    case 'reportDone': {
      const { reportInFlight, shown } = state;
      if (reportInFlight === null) return { state, effects };
      // R2
      let next: MachineState = { ...state, reportInFlight: null };
      if (shown !== null && shown.revision === reportInFlight.revision) {
        next = input.sent
          ? { ...next, reportedSession: reportInFlight.session }
          : { ...next, reportBlockedSession: reportInFlight.session };
      }
      return { state: report(next, effects), effects };
    }
    case 'unmount':
      // F11
      return { state: { ...initialState(), closed: true }, effects };
  }
}

export type ViewState =
  | 'missing'
  | 'skipped'
  | 'loading'
  | 'rendering'
  | 'fetching'
  | 'failed'
  | 'validating'
  | RenderStatus
  | 'fetch_failed';

/** F9: a listed revision newer than the one accepted, which is not (yet) fetchable. */
function newerListed(state: MachineState, entry: PanelEntry): PanelEntry & { kind: 'listed' } | null {
  if (entry.kind !== 'listed') return null;
  return isNewer(entry.summary.revision, state.latestAccepted) ? entry : null;
}

/** The `data-view-state` precedence of the brief's section 5.5. */
export function viewState(state: MachineState, entry: PanelEntry): ViewState {
  if (entry.kind === 'missing') return 'missing';
  if (entry.kind === 'skipped') return 'skipped';
  if (entry.kind === 'loading') return 'loading';
  if (state.rendering !== null) return 'rendering';
  if (state.fetching !== null) return 'fetching';
  const newer = newerListed(state, entry);
  if (newer !== null) {
    const listed = newer.summary.state;
    if (listed === 'failed') return 'failed';
    if (listed === 'validating' || listed === 'fetching' || listed === 'untrusted') return 'validating';
  }
  if (state.shown !== null) return state.shown.status;
  if (state.fetchFailed !== null) return 'fetch_failed';
  return 'loading';
}

function position(error: { line: number | null; column: number | null } | null): string {
  if (error === null || error.line === null) return '';
  return error.column === null ? ` at line ${error.line}` : ` at line ${error.line}, column ${error.column}`;
}

export interface ViewText {
  /** The `role="status"` line, or null. */
  status: string | null;
  /** The `role="alert"` line, or null. */
  error: string | null;
  snippet: string | null;
}

/** The exact copy of the brief's section 5.5. */
export function viewText(state: MachineState, entry: PanelEntry, id: string): ViewText {
  const view = viewState(state, entry);
  const none: ViewText = { status: null, error: null, snippet: null };
  switch (view) {
    case 'missing':
      return { ...none, status: `Panel ${id} is missing` };
    case 'skipped':
      return { ...none, status: `Panel ${id} isn't loaded: ${entry.kind === 'skipped' ? entry.reason : ''}` };
    case 'loading':
      return { ...none, status: 'Loading…' };
    case 'rendering':
    case 'fetching':
      return state.shown === null ? { ...none, status: 'Loading…' } : none;
    case 'validating':
      return { ...none, status: 'Validating…' };
    case 'failed': {
      const error = entry.kind === 'listed' ? entry.summary.error : null;
      return {
        ...none,
        error: `Validation failed${position(error)}${error === null ? '' : `: ${error.message}`}`,
        snippet: error?.snippet ?? null,
      };
    }
    case 'render_failed': {
      const error = state.shown?.error ?? null;
      return {
        ...none,
        error: `Couldn't draw this panel${position(error)}${error === null ? '' : `: ${error.message}`}`,
      };
    }
    case 'fetch_failed':
      return { ...none, error: `Couldn't load panel ${id}: ${state.fetchFailed?.message ?? ''}` };
    case 'rendered':
      return none;
  }
}
