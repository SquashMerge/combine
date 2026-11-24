import { webcrypto } from 'node:crypto';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { OPEN_URL_COMMAND } from '../../src/ipc/names';
import { createTauriOpenUrlPort } from '../../src/ipc/tauri';

beforeAll(() => {
  if (!('crypto' in window)) {
    Object.defineProperty(window, 'crypto', { value: webcrypto });
  }
});

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});

describe('createTauriOpenUrlPort', () => {
  it('invokes open_url with the url', async () => {
    const calls: { cmd: string; args: unknown }[] = [];
    mockIPC(
      (cmd, args) => {
        calls.push({ cmd, args });
      },
      { shouldMockEvents: true },
    );
    await createTauriOpenUrlPort().openUrl('https://example.com/a');
    expect(calls).toEqual([{ cmd: OPEN_URL_COMMAND, args: { url: 'https://example.com/a' } }]);
  });

  it('resolves silently when the command rejects', async () => {
    const spies = [
      vi.spyOn(console, 'error').mockImplementation(() => undefined),
      vi.spyOn(console, 'warn').mockImplementation(() => undefined),
      vi.spyOn(console, 'log').mockImplementation(() => undefined),
    ];
    mockIPC(
      () => {
        throw new Error('refused');
      },
      { shouldMockEvents: true },
    );
    await expect(createTauriOpenUrlPort().openUrl('file:///etc/passwd')).resolves.toBeUndefined();
    for (const spy of spies) expect(spy).not.toHaveBeenCalled();
  });
});
