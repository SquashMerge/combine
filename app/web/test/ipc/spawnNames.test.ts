import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, it } from 'vitest';
import {
  LIST_TEMPLATES_COMMAND,
  SPAWN_EVENT,
  SPAWN_TERMINAL_COMMAND,
} from '../../src/ipc/names';

const contract = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'ipc-contract.json'), 'utf8'),
) as {
  spawnEvent: string;
  commands: { listTemplates: string; spawnTerminal: string };
};

it('matches the spawn keys of the shared ipc contract fixture', () => {
  expect(SPAWN_EVENT).toBe(contract.spawnEvent);
  expect(LIST_TEMPLATES_COMMAND).toBe(contract.commands.listTemplates);
  expect(SPAWN_TERMINAL_COMMAND).toBe(contract.commands.spawnTerminal);
});
