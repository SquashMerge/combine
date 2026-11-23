import { type Dict, emptyDict } from './dict';
import {
  type DecimalU64,
  type HostId,
  ModelError,
  type TermId,
  parseDecimalU64,
  parseTermId,
} from './ids';

export interface TerminalTemplate {
  panel: string;
  title: string | null;
  slot: string;
  argv: string[];
  env: Dict<string>;
  cwd: string;
}

export interface SkippedPanel {
  panel: string;
  reason: string;
}

export interface TemplateList {
  space: string;
  templates: TerminalTemplate[];
  skipped: SkippedPanel[];
  error: string | null;
}

export type SpawnSource = { kind: 'shell' } | { kind: 'template'; panel: string };

export type SpawnEvent =
  | { kind: 'templates'; hostId: string; request: DecimalU64; list: TemplateList }
  | { kind: 'spawned'; hostId: string; request: DecimalU64; space: string; terminal: TermId }
  | { kind: 'error'; hostId: string; request: DecimalU64; code: string; message: string }
  | { kind: 'disconnected'; hostId: string };

function record(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new ModelError(`${what} must be an object`);
  }
  return value as Record<string, unknown>;
}

function str(value: unknown, what: string): string {
  if (typeof value !== 'string') throw new ModelError(`${what} must be a string`);
  return value;
}

function nullableStr(value: unknown, what: string): string | null {
  return value === null ? null : str(value, what);
}

function array(value: unknown, what: string): unknown[] {
  if (!Array.isArray(value)) throw new ModelError(`${what} must be an array`);
  return value;
}

function parseTemplate(value: unknown, what: string): TerminalTemplate {
  const r = record(value, what);
  const env = emptyDict<string>();
  for (const [key, v] of Object.entries(record(r['env'], `${what}.env`))) {
    env[key] = str(v, `${what}.env.${key}`);
  }
  return {
    panel: str(r['panel'], `${what}.panel`),
    title: nullableStr(r['title'], `${what}.title`),
    slot: str(r['slot'], `${what}.slot`),
    argv: array(r['argv'], `${what}.argv`).map((a, i) => str(a, `${what}.argv[${i}]`)),
    env,
    cwd: str(r['cwd'], `${what}.cwd`),
  };
}

function parseSkipped(value: unknown, what: string): SkippedPanel {
  const r = record(value, what);
  return { panel: str(r['panel'], `${what}.panel`), reason: str(r['reason'], `${what}.reason`) };
}

function parseTemplateList(value: unknown): TemplateList {
  const r = record(value, 'template list');
  return {
    space: str(r['space'], 'template list.space'),
    templates: array(r['templates'], 'template list.templates').map((t, i) =>
      parseTemplate(t, `template list.templates[${i}]`),
    ),
    skipped: array(r['skipped'], 'template list.skipped').map((s, i) =>
      parseSkipped(s, `template list.skipped[${i}]`),
    ),
    error: nullableStr(r['error'], 'template list.error'),
  };
}

export function parseSpawnEvent(json: unknown): SpawnEvent {
  const r = record(json, 'spawn event');
  const hostId = str(r['hostId'], 'spawn event.hostId');
  switch (r['kind']) {
    case 'templates':
      return {
        kind: 'templates',
        hostId,
        request: parseDecimalU64(r['request']),
        list: parseTemplateList(r['list']),
      };
    case 'spawned':
      return {
        kind: 'spawned',
        hostId,
        request: parseDecimalU64(r['request']),
        space: str(r['space'], 'spawn event.space'),
        terminal: parseTermId(r['terminal']),
      };
    case 'error':
      return {
        kind: 'error',
        hostId,
        request: parseDecimalU64(r['request']),
        code: str(r['code'], 'spawn event.code'),
        message: str(r['message'], 'spawn event.message'),
      };
    case 'disconnected':
      return { kind: 'disconnected', hostId };
    default:
      throw new ModelError(
        'spawn event.kind must be "templates", "spawned", "error" or "disconnected"',
      );
  }
}

const NEEDS_QUOTES = /[\s"'\\$`\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Zs}]/u;
const SPECIAL = /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Zs}]/u;

const SIMPLE_ESCAPES: Readonly<Record<string, string>> = {
  '\\': '\\\\',
  '"': '\\"',
  $: '\\$',
  '`': '\\`',
  '\n': '\\n',
  '\r': '\\r',
  '\t': '\\t',
};

function escapeCodePoint(ch: string): string {
  const simple = SIMPLE_ESCAPES[ch];
  if (simple !== undefined) return simple;
  if (ch === ' ' || !SPECIAL.test(ch)) return ch;
  const hex = (ch.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0');
  return `\\u{${hex}}`;
}

/**
 * One command-line element exactly as it will be run: plain words stay as they are, anything
 * empty, spaced, quoted or invisible is quoted and escaped so nothing can hide. Never truncates.
 */
export function quoteWord(s: string): string {
  if (s !== '' && !NEEDS_QUOTES.test(s)) return s;
  let out = '';
  for (const ch of s) out += escapeCodePoint(ch);
  return `"${out}"`;
}

export function formatCommandLine(t: {
  argv: readonly string[];
  env: Readonly<Dict<string>>;
}): string {
  const words = Object.entries(t.env).map(([k, v]) => `${quoteWord(k)}=${quoteWord(v)}`);
  for (const a of t.argv) words.push(quoteWord(a));
  return words.join(' ');
}

export class SpawnError extends Error {
  readonly code: string | null;

  constructor(code: string | null, message: string) {
    super(message);
    this.name = 'SpawnError';
    this.code = code;
  }
}

export const SPAWN_WAIT_TIMEOUT_MS = 30_000;
export const EARLY_REPLY_LIMIT = 32;

type ReplyEvent = Exclude<SpawnEvent, { kind: 'disconnected' }>;

interface PendingWait {
  hostId: string;
  resolve: (event: SpawnEvent) => void;
  reject: (error: SpawnError) => void;
  timer: ReturnType<typeof setTimeout>;
}

function replyKey(hostId: string, request: DecimalU64): string {
  return JSON.stringify([hostId, request]);
}

/** Matches each reply to the call that waits for it, whichever of the two comes first. */
export class SpawnReplies {
  private readonly pending = new Map<string, PendingWait>();
  private readonly early = new Map<string, ReplyEvent>();

  wait(hostId: HostId | string, request: DecimalU64): Promise<SpawnEvent> {
    const key = replyKey(hostId, request);
    const buffered = this.early.get(key);
    if (buffered !== undefined) {
      this.early.delete(key);
      return Promise.resolve(buffered);
    }
    return new Promise<SpawnEvent>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(key);
        reject(new SpawnError(null, 'No reply from the host within 30 s.'));
      }, SPAWN_WAIT_TIMEOUT_MS);
      this.pending.set(key, { hostId, resolve, reject, timer });
    });
  }

  onEvent(event: SpawnEvent): void {
    if (event.kind === 'disconnected') {
      for (const [key, wait] of [...this.pending]) {
        if (wait.hostId !== event.hostId) continue;
        clearTimeout(wait.timer);
        this.pending.delete(key);
        wait.reject(new SpawnError(null, 'The host disconnected.'));
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
      wait.resolve(event);
      return;
    }
    this.early.delete(key);
    this.early.set(key, event);
    while (this.early.size > EARLY_REPLY_LIMIT) {
      const oldest = this.early.keys().next();
      if (oldest.done === true) break;
      this.early.delete(oldest.value);
    }
  }
}
