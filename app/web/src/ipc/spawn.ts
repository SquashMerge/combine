import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { parseDecimalU64 } from '../model/ids';
import { SpawnError, SpawnReplies, type SpawnSource, parseSpawnEvent } from '../model/spawn';
import { LIST_TEMPLATES_COMMAND, SPAWN_EVENT, SPAWN_TERMINAL_COMMAND } from './names';
import type { SpawnPort } from './port';

function failureText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createTauriSpawnPort(): SpawnPort {
  const replies = new SpawnReplies();
  let listening: Promise<unknown> | null = null;

  function ensureListening(): Promise<unknown> {
    if (listening === null) {
      const started = listen(SPAWN_EVENT, (event) =>
        replies.onEvent(parseSpawnEvent(event.payload)),
      );
      listening = started;
      started.catch(() => {
        if (listening === started) listening = null;
      });
    }
    return listening;
  }

  async function request(command: string, hostId: string, args: Record<string, unknown>) {
    await ensureListening();
    let id;
    try {
      id = parseDecimalU64(await invoke(command, args));
    } catch (error) {
      throw new SpawnError(null, failureText(error));
    }
    return replies.wait(hostId, id);
  }

  return {
    async listTemplates(hostId, space) {
      const reply = await request(LIST_TEMPLATES_COMMAND, hostId, { hostId, space });
      if (reply.kind === 'templates') return reply.list;
      if (reply.kind === 'error') throw new SpawnError(reply.code, reply.message);
      throw new SpawnError(null, 'Unexpected reply from the host.');
    },
    async spawnTerminal(hostId, space, source: SpawnSource) {
      const reply = await request(SPAWN_TERMINAL_COMMAND, hostId, { hostId, space, source });
      if (reply.kind === 'spawned') return reply.terminal;
      if (reply.kind === 'error') throw new SpawnError(reply.code, reply.message);
      throw new SpawnError(null, 'Unexpected reply from the host.');
    },
  };
}
