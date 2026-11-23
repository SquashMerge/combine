import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, it } from 'vitest';
import {
  CLIENT_STATE_COMMAND,
  HOSTS_EVENT,
  HOSTS_SNAPSHOT_COMMAND,
  LAYOUT_EVENT,
  MUTATE_LAYOUT_COMMAND,
  RETRY_HOST_COMMAND,
  SET_CLIENT_SPACE_STATE_COMMAND,
  SET_LAST_SPACE_COMMAND,
  UNWATCH_LAYOUT_COMMAND,
  WATCH_LAYOUT_COMMAND,
} from '../../src/ipc/names';

const contract = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'ipc-contract.json'), 'utf8'),
) as {
  hostsEvent: string;
  layoutEvent: string;
  commands: {
    snapshot: string;
    retry: string;
    clientState: string;
    setClientSpaceState: string;
    setLastSpace: string;
    watchLayout: string;
    unwatchLayout: string;
    mutateLayout: string;
  };
};

it('matches the shared ipc contract fixture', () => {
  expect(HOSTS_EVENT).toBe(contract.hostsEvent);
  expect(HOSTS_SNAPSHOT_COMMAND).toBe(contract.commands.snapshot);
  expect(RETRY_HOST_COMMAND).toBe(contract.commands.retry);
  expect(CLIENT_STATE_COMMAND).toBe(contract.commands.clientState);
  expect(SET_CLIENT_SPACE_STATE_COMMAND).toBe(contract.commands.setClientSpaceState);
  expect(SET_LAST_SPACE_COMMAND).toBe(contract.commands.setLastSpace);
  expect(LAYOUT_EVENT).toBe(contract.layoutEvent);
  expect(WATCH_LAYOUT_COMMAND).toBe(contract.commands.watchLayout);
  expect(UNWATCH_LAYOUT_COMMAND).toBe(contract.commands.unwatchLayout);
  expect(MUTATE_LAYOUT_COMMAND).toBe(contract.commands.mutateLayout);
});
