import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { parseClientState } from '../model/clientState';
import { parseHostsSnapshot } from '../model/hosts';
import {
  CLIENT_STATE_COMMAND,
  HOSTS_EVENT,
  HOSTS_SNAPSHOT_COMMAND,
  RETRY_HOST_COMMAND,
  SET_CLIENT_SPACE_STATE_COMMAND,
  SET_LAST_SPACE_COMMAND,
} from './names';
import type { ClientStatePort, HostsPort } from './port';

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
