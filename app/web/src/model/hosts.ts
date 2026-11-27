import {
  type DecimalU64,
  type HostId,
  ModelError,
  type TermId,
  parseDecimalU64,
  parseHostId,
  parseTermId,
} from './ids';
import { type SpaceKey, spaceKeyId } from './space';

export type TerminalState = 'running' | 'waiting' | 'idle' | 'exited' | 'unknown';

export type ConnectionState =
  | { kind: 'connecting' }
  | { kind: 'connected' }
  | { kind: 'dropped'; reason: string; detail: string | null; retryInMs: number }
  | { kind: 'reconnecting'; attempt: number }
  | { kind: 'failed'; message: string; detail: string | null };

export interface TerminalSummary {
  id: TermId;
  name: string;
  command: string;
  state: TerminalState;
}

export interface SpaceView {
  key: SpaceKey;
  terminals: TerminalSummary[];
}

/** The backend that owns a host's terminals. `instance` tells one backend of that kind from another. */
export interface BackendView {
  id: string;
  instance: string | null;
}

export interface HostView {
  name: string;
  hostId: HostId | null;
  connection: ConnectionState;
  spaces: SpaceView[];
  /** `null` until the host's first terminal list has arrived. */
  backend: BackendView | null;
}

export interface ConfigStatus {
  path: string;
  error: string | null;
}

export interface HostsSnapshot {
  revision: DecimalU64;
  config: ConfigStatus;
  hosts: HostView[];
}

const TERMINAL_STATES: readonly string[] = ['running', 'waiting', 'idle', 'exited', 'unknown'];
const U32_MAX = 4294967295;

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

function u32(value: unknown, what: string): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > U32_MAX) {
    throw new ModelError(`${what} must be an unsigned 32-bit integer`);
  }
  return value;
}

function array(value: unknown, what: string): unknown[] {
  if (!Array.isArray(value)) throw new ModelError(`${what} must be an array`);
  return value;
}

function parseConnection(value: unknown, what: string): ConnectionState {
  const r = record(value, what);
  switch (r['kind']) {
    case 'connecting':
      return { kind: 'connecting' };
    case 'connected':
      return { kind: 'connected' };
    case 'dropped':
      return {
        kind: 'dropped',
        reason: str(r['reason'], `${what}.reason`),
        detail: nullableStr(r['detail'], `${what}.detail`),
        retryInMs: u32(r['retryInMs'], `${what}.retryInMs`),
      };
    case 'reconnecting':
      return { kind: 'reconnecting', attempt: u32(r['attempt'], `${what}.attempt`) };
    case 'failed':
      return {
        kind: 'failed',
        message: str(r['message'], `${what}.message`),
        detail: nullableStr(r['detail'], `${what}.detail`),
      };
    default:
      throw new ModelError(`${what}.kind is not a known connection state`);
  }
}

function parseTerminal(value: unknown, what: string): TerminalSummary {
  const r = record(value, what);
  const state = str(r['state'], `${what}.state`);
  if (!TERMINAL_STATES.includes(state)) {
    throw new ModelError(`${what}.state is not a known terminal state`);
  }
  return {
    id: parseTermId(r['id']),
    name: str(r['name'], `${what}.name`),
    command: str(r['command'], `${what}.command`),
    state: state as TerminalState,
  };
}

function parseSpace(value: unknown, what: string): SpaceView {
  const r = record(value, what);
  const key = record(r['key'], `${what}.key`);
  return {
    key: { hostId: parseHostId(key['hostId']), path: str(key['path'], `${what}.key.path`) },
    terminals: array(r['terminals'], `${what}.terminals`).map((t, i) =>
      parseTerminal(t, `${what}.terminals[${i}]`),
    ),
  };
}

function parseBackend(value: unknown, what: string): BackendView | null {
  if (value === null) return null;
  const r = record(value, what);
  return {
    id: str(r['id'], `${what}.id`),
    instance: nullableStr(r['instance'], `${what}.instance`),
  };
}

function parseHost(value: unknown, what: string): HostView {
  const r = record(value, what);
  const hostId = r['hostId'] === null ? null : parseHostId(r['hostId']);
  const host: HostView = {
    name: str(r['name'], `${what}.name`),
    hostId,
    connection: parseConnection(r['connection'], `${what}.connection`),
    spaces: array(r['spaces'], `${what}.spaces`).map((s, i) => parseSpace(s, `${what}.spaces[${i}]`)),
    backend: parseBackend(r['backend'], `${what}.backend`),
  };
  if (host.hostId === null && host.spaces.length > 0) {
    throw new ModelError(`${what} has spaces but no hostId`);
  }
  for (const space of host.spaces) {
    if (space.key.hostId !== host.hostId) {
      throw new ModelError(`${what} has a space keyed by another host: ${spaceKeyId(space.key)}`);
    }
  }
  return host;
}

export function parseHostsSnapshot(json: unknown): HostsSnapshot {
  const r = record(json, 'snapshot');
  const config = record(r['config'], 'snapshot.config');
  const hosts = array(r['hosts'], 'snapshot.hosts').map((h, i) =>
    parseHost(h, `snapshot.hosts[${i}]`),
  );
  const seen = new Set<string>();
  for (const host of hosts) {
    if (host.hostId === null) continue;
    if (seen.has(host.hostId)) throw new ModelError(`two hosts share hostId ${host.hostId}`);
    seen.add(host.hostId);
  }
  return {
    revision: parseDecimalU64(r['revision']),
    config: {
      path: str(config['path'], 'snapshot.config.path'),
      error: nullableStr(config['error'], 'snapshot.config.error'),
    },
    hosts,
  };
}

export function isConnected(host: HostView): boolean {
  return host.connection.kind === 'connected';
}
