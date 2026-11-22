import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, it } from 'vitest';
import { HOSTS_EVENT, HOSTS_SNAPSHOT_COMMAND, RETRY_HOST_COMMAND } from '../../src/ipc/names';

const contract = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'ipc-contract.json'), 'utf8'),
) as { hostsEvent: string; commands: { snapshot: string; retry: string } };

it('matches the shared ipc contract fixture', () => {
  expect(HOSTS_EVENT).toBe(contract.hostsEvent);
  expect(HOSTS_SNAPSHOT_COMMAND).toBe(contract.commands.snapshot);
  expect(RETRY_HOST_COMMAND).toBe(contract.commands.retry);
});
