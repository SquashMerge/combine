import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { parseHostsSnapshot } from '../model/hosts';
import { HOSTS_EVENT, HOSTS_SNAPSHOT_COMMAND, RETRY_HOST_COMMAND } from './names';
import type { HostsPort } from './port';

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
