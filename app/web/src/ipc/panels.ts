import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { type HostId, parseDecimalU64 } from '../model/ids';
import {
  PanelFetchError,
  PanelFetchQueue,
  PanelReplies,
  type PanelsEvent,
  parsePanelsChanged,
} from '../model/panels';
import {
  CLOSE_PANEL_COMMAND,
  GET_PANEL_COMMAND,
  PANELS_EVENT,
  PIN_PANEL_COMMAND,
  REPORT_RENDER_COMMAND,
} from './names';
import type { PanelPort } from './port';

function failureText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createTauriPanelPort(): PanelPort {
  const replies = new PanelReplies();
  const listeners = new Set<(event: PanelsEvent) => void>();
  let listening: Promise<unknown> | null = null;
  // Request ids restart in each host session, so an invoke that began before a disconnect must not
  // register a wait, or take a reply, under an id a later session is using.
  const generations = new Map<string, number>();
  const generation = (hostId: string): number => generations.get(hostId) ?? 0;

  const queue = new PanelFetchQueue(async (hostId, space, panel) => {
    await ensureListening();
    const started = generation(hostId);
    let id;
    try {
      id = parseDecimalU64(await invoke(GET_PANEL_COMMAND, { hostId, space, panel }));
    } catch (error) {
      throw new PanelFetchError(null, failureText(error));
    }
    if (generation(hostId) !== started) throw new PanelFetchError(null, 'The host disconnected.');
    return replies.wait(hostId, id);
  });

  async function act(
    command: string,
    hostId: HostId,
    space: string,
    panel: string,
  ): Promise<void> {
    await ensureListening();
    const started = generation(hostId);
    let id;
    try {
      id = parseDecimalU64(await invoke(command, { hostId, space, panel }));
    } catch (error) {
      throw new PanelFetchError(null, failureText(error));
    }
    if (generation(hostId) !== started) throw new PanelFetchError(null, 'The host disconnected.');
    await replies.waitDone(hostId, id);
  }

  function ensureListening(): Promise<unknown> {
    if (listening === null) {
      const started = listen(PANELS_EVENT, (event) => {
        const parsed = parsePanelsChanged(event.payload);
        replies.onEvent(parsed);
        if (parsed.kind === 'disconnected') {
          generations.set(parsed.hostId, generation(parsed.hostId) + 1);
          queue.disconnected(parsed.hostId as HostId);
        }
        if (parsed.kind === 'list' || parsed.kind === 'disconnected') {
          for (const listener of [...listeners]) listener(parsed);
        }
      });
      listening = started;
      started.catch(() => {
        if (listening === started) listening = null;
      });
    }
    return listening;
  }

  return {
    async onPanelsEvent(listener) {
      await ensureListening();
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    fetchPanel: (hostId, space, panel, urgent) => queue.fetch(hostId, space, panel, urgent),
    closePanel: (hostId, space, panel) => act(CLOSE_PANEL_COMMAND, hostId, space, panel),
    pinPanel: (hostId, space, panel) => act(PIN_PANEL_COMMAND, hostId, space, panel),
    async reportRender(hostId, report) {
      try {
        await invoke(REPORT_RENDER_COMMAND, { hostId, report });
        return true;
      } catch {
        return false;
      }
    },
  };
}
