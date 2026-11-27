import { type DecimalU64, type HostId, ModelError, asRecord, parseDecimalU64 } from './ids';
import type { RendererName } from './render';
import type { SkippedPanel } from './spawn';

export type PanelState =
  | 'validating'
  | 'fetching'
  | 'validated'
  | 'validated_parser_only'
  | 'failed'
  | 'untrusted'
  | 'removed';

export interface PanelError {
  code: string;
  message: string;
  line: number | null;
  column: number | null;
  snippet: string | null;
}

export type CreatedBy = { type: 'user' } | { type: 'agent'; slot: string | null };

/** Where a panel lives: in the project's `.workspace/`, or in the daemon's own data. */
export type PanelLifetime = 'persistent' | 'scratch';

export interface PanelSummary {
  id: string;
  title: string | null;
  renderer: RendererName;
  revision: DecimalU64;
  state: PanelState;
  error: PanelError | null;
  createdBy: CreatedBy | null;
  lifetime: PanelLifetime;
}

export interface PanelList {
  space: string;
  panels: PanelSummary[];
  skipped: SkippedPanel[];
  error: string | null;
}

export interface PanelContent {
  space: string;
  id: string;
  revision: DecimalU64;
  renderer: RendererName;
  source: string;
  state: PanelState;
  error: PanelError | null;
}

export type RenderStatus = 'rendered' | 'render_failed';

export interface RenderReport {
  space: string;
  panel: string;
  revision: DecimalU64;
  status: RenderStatus;
  error: PanelError | null;
}

export type PanelsChanged =
  | { kind: 'list'; hostId: string; request: DecimalU64 | null; list: PanelList }
  | { kind: 'panel'; hostId: string; request: DecimalU64; panel: PanelContent }
  | { kind: 'closed' | 'pinned'; hostId: string; request: DecimalU64; space: string; panel: string }
  | { kind: 'error'; hostId: string; request: DecimalU64; code: string; message: string }
  | { kind: 'disconnected'; hostId: string };

export type PanelsEvent = Extract<PanelsChanged, { kind: 'list' | 'disconnected' }>;

/** What the host sends for a space whose panels it has not read yet. */
export const PANELS_LOADING = "the space's panels are loading";

const PANEL_STATES: readonly PanelState[] = [
  'validating',
  'fetching',
  'validated',
  'validated_parser_only',
  'failed',
  'untrusted',
  'removed',
];

function str(value: unknown, what: string): string {
  if (typeof value !== 'string') throw new ModelError(`${what} must be a string`);
  return value;
}

function nullableStr(value: unknown, what: string): string | null {
  return value === null ? null : str(value, what);
}

function nullableCount(value: unknown, what: string): number | null {
  if (value === null) return null;
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) {
    throw new ModelError(`${what} must be a non-negative integer or null`);
  }
  return value;
}

function array(value: unknown, what: string): unknown[] {
  if (!Array.isArray(value)) throw new ModelError(`${what} must be an array`);
  return value;
}

function renderer(value: unknown, what: string): RendererName {
  if (value !== 'mermaid' && value !== 'markdown') {
    throw new ModelError(`${what} must be "mermaid" or "markdown"`);
  }
  return value;
}

function panelState(value: unknown, what: string): PanelState {
  const found = PANEL_STATES.find((state) => state === value);
  if (found === undefined) throw new ModelError(`${what} is not a panel state`);
  return found;
}

function parseError(value: unknown, what: string): PanelError | null {
  if (value === null) return null;
  const r = asRecord(value, what);
  return {
    code: str(r['code'], `${what}.code`),
    message: str(r['message'], `${what}.message`),
    line: nullableCount(r['line'], `${what}.line`),
    column: nullableCount(r['column'], `${what}.column`),
    snippet: nullableStr(r['snippet'], `${what}.snippet`),
  };
}

function parseCreatedBy(value: unknown, what: string): CreatedBy | null {
  if (value === null) return null;
  const r = asRecord(value, what);
  switch (r['type']) {
    case 'user':
      return { type: 'user' };
    case 'agent':
      return { type: 'agent', slot: nullableStr(r['slot'], `${what}.slot`) };
    default:
      throw new ModelError(`${what}.type must be "user" or "agent"`);
  }
}

function parseLifetime(value: unknown, what: string): PanelLifetime {
  if (value !== 'persistent' && value !== 'scratch') {
    throw new ModelError(`${what} must be "persistent" or "scratch"`);
  }
  return value;
}

function parseSummary(value: unknown, what: string): PanelSummary {
  const r = asRecord(value, what);
  return {
    id: str(r['id'], `${what}.id`),
    title: nullableStr(r['title'], `${what}.title`),
    renderer: renderer(r['renderer'], `${what}.renderer`),
    revision: parseDecimalU64(r['revision']),
    state: panelState(r['state'], `${what}.state`),
    error: parseError(r['error'], `${what}.error`),
    createdBy: parseCreatedBy(r['createdBy'], `${what}.createdBy`),
    lifetime: parseLifetime(r['lifetime'], `${what}.lifetime`),
  };
}

function parseSkipped(value: unknown, what: string): SkippedPanel {
  const r = asRecord(value, what);
  return { panel: str(r['panel'], `${what}.panel`), reason: str(r['reason'], `${what}.reason`) };
}

function parseList(value: unknown): PanelList {
  const r = asRecord(value, 'panel list');
  return {
    space: str(r['space'], 'panel list.space'),
    panels: array(r['panels'], 'panel list.panels').map((p, i) =>
      parseSummary(p, `panel list.panels[${i}]`),
    ),
    skipped: array(r['skipped'], 'panel list.skipped').map((s, i) =>
      parseSkipped(s, `panel list.skipped[${i}]`),
    ),
    error: nullableStr(r['error'], 'panel list.error'),
  };
}

function parseContent(value: unknown): PanelContent {
  const r = asRecord(value, 'panel content');
  return {
    space: str(r['space'], 'panel content.space'),
    id: str(r['id'], 'panel content.id'),
    revision: parseDecimalU64(r['revision']),
    renderer: renderer(r['renderer'], 'panel content.renderer'),
    source: str(r['source'], 'panel content.source'),
    state: panelState(r['state'], 'panel content.state'),
    error: parseError(r['error'], 'panel content.error'),
  };
}

export function parsePanelsChanged(json: unknown): PanelsChanged {
  const r = asRecord(json, 'panels event');
  const hostId = str(r['hostId'], 'panels event.hostId');
  switch (r['kind']) {
    case 'list':
      return {
        kind: 'list',
        hostId,
        request: r['request'] === null ? null : parseDecimalU64(r['request']),
        list: parseList(r['list']),
      };
    case 'panel':
      return {
        kind: 'panel',
        hostId,
        request: parseDecimalU64(r['request']),
        panel: parseContent(r['panel']),
      };
    case 'closed':
    case 'pinned':
      return {
        kind: r['kind'],
        hostId,
        request: parseDecimalU64(r['request']),
        space: str(r['space'], 'panels event.space'),
        panel: str(r['panel'], 'panels event.panel'),
      };
    case 'error':
      return {
        kind: 'error',
        hostId,
        request: parseDecimalU64(r['request']),
        code: str(r['code'], 'panels event.code'),
        message: str(r['message'], 'panels event.message'),
      };
    case 'disconnected':
      return { kind: 'disconnected', hostId };
    default:
      throw new ModelError('panels event.kind must be "list", "panel", "closed", "pinned", "error" or "disconnected"');
  }
}

export function isLoadingList(list: PanelList): boolean {
  return list.panels.length === 0 && list.skipped.length === 0 && list.error === PANELS_LOADING;
}

export type PanelEntry =
  | { kind: 'loading' }
  | { kind: 'missing' }
  | { kind: 'skipped'; reason: string }
  | { kind: 'listed'; summary: PanelSummary };

/** What the space's panel list says about one tab ID. A null list is one not yet received. */
export function panelEntry(list: PanelList | null, id: string): PanelEntry {
  if (list === null) return { kind: 'loading' };
  const summary = list.panels.find((panel) => panel.id === id);
  if (summary !== undefined && summary.state !== 'removed') return { kind: 'listed', summary };
  const skipped = list.skipped.find((entry) => entry.panel === id);
  if (skipped !== undefined) return { kind: 'skipped', reason: skipped.reason };
  return { kind: 'missing' };
}

/** The listed panel's title, or the ID when it has none or is not listed. */
export function tabLabel(list: PanelList | null, id: string): string {
  const entry = panelEntry(list, id);
  return entry.kind === 'listed' ? (entry.summary.title ?? id) : id;
}

/**
 * The selected space's panels as the shell holds them. `session` goes up each time the host's
 * connection ends; it is not the terminal stream's epoch.
 */
export interface SpacePanelsView {
  session: number;
  connected: boolean;
  list: PanelList | null;
}

export class PanelFetchError extends Error {
  readonly code: string | null;

  constructor(code: string | null, message: string) {
    super(message);
    this.name = 'PanelFetchError';
    this.code = code;
  }
}

export const PANEL_FETCH_TIMEOUT_MS = 30_000;
export const EARLY_PANEL_REPLY_LIMIT = 32;

type ReplyEvent = Exclude<PanelsChanged, { kind: 'list' | 'disconnected' }>;

interface PendingWait {
  hostId: string;
  resolve: (event: ReplyEvent) => void;
  reject: (error: PanelFetchError) => void;
  timer: ReturnType<typeof setTimeout>;
}

function replyKey(hostId: string, request: DecimalU64): string {
  return JSON.stringify([hostId, request]);
}

/**
 * Matches each reply to a `get_panel`, `close_panel` or `pin_panel` request to the call that
 * waits for it, whichever of the two comes first.
 */
export class PanelReplies {
  private readonly pending = new Map<string, PendingWait>();
  private readonly early = new Map<string, ReplyEvent>();

  /** Resolves with the panel a `get_panel` request asked for. */
  async wait(hostId: HostId | string, request: DecimalU64): Promise<PanelContent> {
    const event = await this.waitEvent(hostId, request);
    if (event.kind === 'panel') return event.panel;
    throw unexpectedReply();
  }

  /** Resolves when a `close_panel` or `pin_panel` request is answered. */
  async waitDone(hostId: HostId | string, request: DecimalU64): Promise<void> {
    const event = await this.waitEvent(hostId, request);
    if (event.kind === 'closed' || event.kind === 'pinned') return;
    throw unexpectedReply();
  }

  private waitEvent(hostId: HostId | string, request: DecimalU64): Promise<ReplyEvent> {
    const key = replyKey(hostId, request);
    const buffered = this.early.get(key);
    if (buffered !== undefined) {
      this.early.delete(key);
      return settled(buffered);
    }
    const displaced = this.pending.get(key);
    if (displaced !== undefined) {
      clearTimeout(displaced.timer);
      this.pending.delete(key);
      displaced.reject(new PanelFetchError(null, 'Another request took this reply.'));
    }
    return new Promise<ReplyEvent>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(key);
        reject(new PanelFetchError(null, 'No reply from the host within 30 s.'));
      }, PANEL_FETCH_TIMEOUT_MS);
      this.pending.set(key, { hostId, resolve, reject, timer });
    });
  }

  onEvent(event: PanelsChanged): void {
    if (event.kind === 'list') return;
    if (event.kind === 'disconnected') {
      for (const [key, wait] of [...this.pending]) {
        if (wait.hostId !== event.hostId) continue;
        clearTimeout(wait.timer);
        this.pending.delete(key);
        wait.reject(new PanelFetchError(null, 'The host disconnected.'));
      }
      for (const [key, buffered] of [...this.early]) {
        if (buffered.hostId === event.hostId) this.early.delete(key);
      }
      return;
    }
    const key = replyKey(event.hostId, event.request);
    const wait = this.pending.get(key);
    if (wait !== undefined) {
      clearTimeout(wait.timer);
      this.pending.delete(key);
      if (event.kind === 'error') wait.reject(new PanelFetchError(event.code, event.message));
      else wait.resolve(event);
      return;
    }
    this.early.delete(key);
    this.early.set(key, event);
    while (this.early.size > EARLY_PANEL_REPLY_LIMIT) {
      const oldest = this.early.keys().next();
      if (oldest.done === true) break;
      this.early.delete(oldest.value);
    }
  }
}

function settled(event: ReplyEvent): Promise<ReplyEvent> {
  return event.kind === 'error'
    ? Promise.reject(new PanelFetchError(event.code, event.message))
    : Promise.resolve(event);
}

function unexpectedReply(): PanelFetchError {
  return new PanelFetchError(null, 'The host sent an unexpected reply.');
}

export const PANEL_FETCHES_IN_FLIGHT = 4;

interface Waiting {
  space: string;
  panel: string;
  urgent: boolean;
  resolve: (content: PanelContent) => void;
  reject: (error: PanelFetchError) => void;
}

interface HostQueue {
  inFlight: number;
  waiting: Waiting[];
}

function waitingKey(space: string, panel: string): string {
  return JSON.stringify([space, panel]);
}

/**
 * Keeps at most `limit` `get_panel` calls unsettled per host, so a screen of panel tiles never
 * has more replies outstanding than the daemon tolerates. The rest wait: urgent ones first, then
 * in call order. A waiting call is replaced by a newer one for the same panel; a sent one is not.
 */
export class PanelFetchQueue {
  private readonly hosts = new Map<string, HostQueue>();

  constructor(
    private readonly send: (hostId: HostId, space: string, panel: string) => Promise<PanelContent>,
    private readonly limit: number = PANEL_FETCHES_IN_FLIGHT,
  ) {}

  fetch(hostId: HostId, space: string, panel: string, urgent: boolean): Promise<PanelContent> {
    const host = this.hostQueue(hostId);
    return new Promise<PanelContent>((resolve, reject) => {
      const call: Waiting = { space, panel, urgent, resolve, reject };
      const key = waitingKey(space, panel);
      const at = host.waiting.findIndex((w) => waitingKey(w.space, w.panel) === key);
      const replaced = at < 0 ? undefined : host.waiting[at];
      if (replaced === undefined) {
        host.waiting.push(call);
      } else {
        call.urgent = urgent || replaced.urgent;
        host.waiting[at] = call;
        replaced.reject(new PanelFetchError('superseded', 'A newer request replaced this one.'));
      }
      this.pump(hostId, host);
    });
  }

  /** The host's connection ended: its waiting calls fail; the sent ones fail with their replies. */
  disconnected(hostId: HostId): void {
    const host = this.hosts.get(hostId);
    if (host === undefined) return;
    const rejected = host.waiting;
    host.waiting = [];
    for (const call of rejected) call.reject(new PanelFetchError(null, 'The host disconnected.'));
  }

  inFlight(hostId: HostId): number {
    return this.hosts.get(hostId)?.inFlight ?? 0;
  }

  private hostQueue(hostId: HostId): HostQueue {
    let host = this.hosts.get(hostId);
    if (host === undefined) {
      host = { inFlight: 0, waiting: [] };
      this.hosts.set(hostId, host);
    }
    return host;
  }

  private pump(hostId: HostId, host: HostQueue): void {
    while (host.inFlight < this.limit && host.waiting.length > 0) {
      const urgent = host.waiting.findIndex((call) => call.urgent);
      const [next] = host.waiting.splice(urgent < 0 ? 0 : urgent, 1);
      if (next === undefined) return;
      host.inFlight += 1;
      const done = (): void => {
        host.inFlight -= 1;
        this.pump(hostId, host);
      };
      let sent: Promise<PanelContent>;
      try {
        sent = this.send(hostId, next.space, next.panel);
      } catch (cause) {
        sent = Promise.reject(cause);
      }
      sent.then(
        (content) => {
          done();
          next.resolve(content);
        },
        (cause: unknown) => {
          done();
          if (cause instanceof PanelFetchError) next.reject(cause);
          else next.reject(new PanelFetchError(null, cause instanceof Error ? cause.message : String(cause)));
        },
      );
    }
  }
}
