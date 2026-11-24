import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { parseClientState } from '../model/clientState';
import { parseHostsSnapshot } from '../model/hosts';
import { parseDecimalU64 } from '../model/ids';
import { parseLayoutEvent } from '../model/layout';
import {
  CLIENT_STATE_COMMAND,
  HOSTS_EVENT,
  HOSTS_SNAPSHOT_COMMAND,
  LAYOUT_EVENT,
  MUTATE_LAYOUT_COMMAND,
  OPEN_URL_COMMAND,
  RETRY_HOST_COMMAND,
  SET_CLIENT_SPACE_STATE_COMMAND,
  SET_LAST_SPACE_COMMAND,
  UNWATCH_LAYOUT_COMMAND,
  WATCH_LAYOUT_COMMAND,
} from './names';
import type { ClientStatePort, HostsPort, LayoutPort, OpenUrlPort } from './port';

export function createTauriHostsPort(): HostsPort {
  return {
    async getSnapshot() {
      return parseHostsSnapshot(await invoke(HOSTS_SNAPSHOT_COMMAND));
    },
    onSnapshot(listener) {
      return listen(HOSTS_EVENT, (event) => listener(parseHostsSnapshot(event.payload)));
    },
    async retryHost(name) {
      await invoke(RETRY_HOST_COMMAND, { name });
    },
  };
}

export function createTauriClientStatePort(): ClientStatePort {
  return {
    async load(hostId) {
      return parseClientState(await invoke(CLIENT_STATE_COMMAND, { hostId }));
    },
    async setSpaceState(hostId, path, state) {
      await invoke(SET_CLIENT_SPACE_STATE_COMMAND, { hostId, path, state });
    },
    async setLastSpace(hostId, spaceId, selectedAtMs) {
      await invoke(SET_LAST_SPACE_COMMAND, { hostId, spaceId, selectedAtMs });
    },
  };
}

export function createTauriLayoutPort(): LayoutPort {
  return {
    async watchLayout(hostId, space) {
      return parseDecimalU64(await invoke(WATCH_LAYOUT_COMMAND, { hostId, space }));
    },
    async unwatchLayout(hostId) {
      await invoke(UNWATCH_LAYOUT_COMMAND, { hostId });
    },
    async mutateLayout(hostId, space, baseLayoutRevision, ops) {
      return parseDecimalU64(
        await invoke(MUTATE_LAYOUT_COMMAND, { hostId, space, baseLayoutRevision, ops }),
      );
    },
    onLayoutEvent(listener) {
      return listen(LAYOUT_EVENT, (event) => listener(parseLayoutEvent(event.payload)));
    },
  };
}

export function createTauriOpenUrlPort(): OpenUrlPort {
  return {
    async openUrl(url) {
      try {
        await invoke(OPEN_URL_COMMAND, { url });
      } catch {
        // Refused or failed: the link stays inert.
      }
    },
  };
}
