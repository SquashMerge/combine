import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { DecimalU64 } from '../../src/model/ids';
import { ModelError } from '../../src/model/ids';
import {
  type SpawnEvent,
  SpawnError,
  SpawnReplies,
  formatCommandLine,
  parseSpawnEvent,
  quoteWord,
} from '../../src/model/spawn';

interface Fixture {
  templates: unknown;
  spawned: unknown;
  error: unknown;
  disconnected: unknown;
  sources: unknown[];
}

const fixture = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'spawn-event.json'), 'utf8'),
) as Fixture;

const req = (n: string): DecimalU64 => n as DecimalU64;

describe('parseSpawnEvent', () => {
  it('parses_fixture_events', () => {
    const templates = parseSpawnEvent(fixture.templates);
    expect(templates.kind).toBe('templates');
    if (templates.kind !== 'templates') throw new Error('unreachable');
    expect(templates.request).toBe('9007199254740993');
    expect(templates.list.space).toBe('/work');
    expect(templates.list.templates[0]?.title).toBe('Claude');
    expect(templates.list.skipped).toEqual([{ panel: 'notes', reason: 'not a terminal panel' }]);
    expect(templates.list.error).toBeNull();
    expect(parseSpawnEvent(fixture.spawned)).toEqual({
      kind: 'spawned',
      hostId: 'host-1',
      request: '8',
      space: '/work',
      terminal: 'hc:t12',
    });
    expect(parseSpawnEvent(fixture.error)).toEqual({
      kind: 'error',
      hostId: 'host-1',
      request: '9',
      code: 'cwd_refused',
      message: 'the template\'s cwd "../x" has a .. segment',
    });
    expect(parseSpawnEvent(fixture.disconnected)).toEqual({ kind: 'disconnected', hostId: 'host-1' });
  });

  it('parse_keeps_argv_env_cwd_exact', () => {
    const odd = ' a‮b\n', key = '__proto__';
    const json = JSON.parse(
      `{"kind":"templates","hostId":"h","request":"1","list":{"space":"/s","templates":[` +
        `{"panel":"p","title":null,"slot":"p","argv":["", "${odd.replace('\n', '\\n')}"],` +
        `"env":{"${key}":"\\u0007 v "},"cwd":" ./x "}],"skipped":[],"error":"boom"}}`,
    ) as unknown;
    const event = parseSpawnEvent(json);
    if (event.kind !== 'templates') throw new Error('unreachable');
    const t = event.list.templates[0];
    expect(t?.argv).toEqual(['', odd]);
    expect(Object.getPrototypeOf(t?.env)).toBeNull();
    expect(Object.entries(t?.env ?? {})).toEqual([[key, '\u0007 v ']]);
    expect(t?.cwd).toBe(' ./x ');
    expect(t?.title).toBeNull();
    expect(event.list.error).toBe('boom');
  });

  it('rejects_bad_shapes', () => {
    const bad: unknown[] = [
      null,
      [],
      {},
      { kind: 'nope', hostId: 'h' },
      { kind: 'disconnected' },
      { kind: 'spawned', hostId: 'h', request: 8, space: '/w', terminal: 'hc:t1' },
      { kind: 'spawned', hostId: 'h', request: '08', space: '/w', terminal: 'hc:t1' },
      { kind: 'spawned', hostId: 'h', request: '8', space: '/w', terminal: '' },
      { kind: 'error', hostId: 'h', request: '9', code: 1, message: 'm' },
      { kind: 'templates', hostId: 'h', request: '1', list: { space: '/s', templates: [], skipped: [] } },
      {
        kind: 'templates',
        hostId: 'h',
        request: '1',
        list: {
          space: '/s',
          templates: [{ panel: 'p', title: null, slot: 's', argv: ['a'], env: { K: 1 }, cwd: '.' }],
          skipped: [],
          error: null,
        },
      },
      {
        kind: 'templates',
        hostId: 'h',
        request: '1',
        list: {
          space: '/s',
          templates: [{ panel: 'p', title: null, slot: 's', argv: [1], env: {}, cwd: '.' }],
          skipped: [],
          error: null,
        },
      },
    ];
    for (const value of bad) expect(() => parseSpawnEvent(value)).toThrow(ModelError);
  });
});

describe('quoteWord', () => {
  it('quoteWord_leaves_plain_words', () => {
    for (const word of ['claude', '--resume', 'a/b.c-d_e=f', 'ünï', '中文']) {
      expect(quoteWord(word)).toBe(word);
    }
  });

  it('quoteWord_quotes_empty_element', () => {
    expect(quoteWord('')).toBe('""');
  });

  it('quoteWord_escapes_newline_tab_cr', () => {
    expect(quoteWord('a\nb\rc\td')).toBe('"a\\nb\\rc\\td"');
  });

  it('quoteWord_escapes_bidi_and_format', () => {
    expect(quoteWord('a‮b')).toBe('"a\\u{202E}b"');
    expect(quoteWord('a​b')).toBe('"a\\u{200B}b"');
    expect(quoteWord('﻿a')).toBe('"\\u{FEFF}a"');
    expect(quoteWord('a\u{e0001}')).toBe('"a\\u{E0001}"');
  });

  it('quoteWord_escapes_controls', () => {
    expect(quoteWord('a\u0007b')).toBe('"a\\u{0007}b"');
    expect(quoteWord('a\u0085b')).toBe('"a\\u{0085}b"');
  });

  it('quoteWord_escapes_quotes_backslash_dollar_backtick', () => {
    expect(quoteWord('say "hi"')).toBe('"say \\"hi\\""');
    expect(quoteWord('a\\b')).toBe('"a\\\\b"');
    expect(quoteWord('$HOME')).toBe('"\\$HOME"');
    expect(quoteWord('`id`')).toBe('"\\`id\\`"');
    expect(quoteWord("it's")).toBe('"it\'s"');
  });

  it('quoteWord_keeps_space_and_escapes_nbsp', () => {
    expect(quoteWord('a b')).toBe('"a b"');
    expect(quoteWord('a b')).toBe('"a\\u{00A0}b"');
    expect(quoteWord('a b')).toBe('"a\\u{2028}b"');
  });
});

describe('formatCommandLine', () => {
  it('formatCommandLine_never_truncates', () => {
    const long = 'x'.repeat(10_000);
    const line = formatCommandLine({ argv: ['run', long, `${long} y`], env: {} });
    expect(line).toBe(`run ${long} "${long} y"`);
  });

  it('formatCommandLine_quotes_env_keys_and_values', () => {
    const env = { FOO: 'bar baz', 'K‮EY': '', PLAIN: 'ok' };
    expect(formatCommandLine({ argv: ['claude', '', 'a b'], env })).toBe(
      'FOO="bar baz" "K\\u{202E}EY"="" PLAIN=ok claude "" "a b"',
    );
  });
});

describe('SpawnReplies', () => {
  afterEach(() => vi.useRealTimers());

  const spawned = (hostId: string, request: string): SpawnEvent => ({
    kind: 'spawned',
    hostId,
    request: req(request),
    space: '/w',
    terminal: 'hc:t1' as never,
  });

  it('replies_early_reply_buffered', async () => {
    const replies = new SpawnReplies();
    replies.onEvent(spawned('h', '1'));
    await expect(replies.wait('h', req('1'))).resolves.toEqual(spawned('h', '1'));

    const waiting = replies.wait('h', req('2'));
    replies.onEvent(spawned('h', '2'));
    await expect(waiting).resolves.toEqual(spawned('h', '2'));

    for (let i = 0; i < 33; i += 1) replies.onEvent(spawned('h', String(100 + i)));
    vi.useFakeTimers();
    const evicted = replies.wait('h', req('100'));
    const evictedResult = expect(evicted).rejects.toBeInstanceOf(SpawnError);
    await vi.advanceTimersByTimeAsync(30_000);
    await evictedResult;
    await expect(replies.wait('h', req('101'))).resolves.toEqual(spawned('h', '101'));
  });

  it('disconnect_rejects', async () => {
    const replies = new SpawnReplies();
    replies.onEvent(spawned('h', '5'));
    const a = replies.wait('h', req('1'));
    const other = replies.wait('other', req('1'));
    replies.onEvent({ kind: 'disconnected', hostId: 'h' });
    await expect(a).rejects.toMatchObject({ code: null, message: 'The host disconnected.' });
    replies.onEvent(spawned('other', '1'));
    await expect(other).resolves.toEqual(spawned('other', '1'));

    vi.useFakeTimers();
    const gone = replies.wait('h', req('5'));
    const goneResult = expect(gone).rejects.toBeInstanceOf(SpawnError);
    await vi.advanceTimersByTimeAsync(30_000);
    await goneResult;
  });

  it('timeout_rejects', async () => {
    vi.useFakeTimers();
    const replies = new SpawnReplies();
    const waiting = replies.wait('h', req('1'));
    const result = expect(waiting).rejects.toMatchObject({
      code: null,
      message: 'No reply from the host within 30 s.',
    });
    await vi.advanceTimersByTimeAsync(29_999);
    replies.onEvent({ kind: 'disconnected', hostId: 'nobody' });
    await vi.advanceTimersByTimeAsync(1);
    await result;
  });
});
