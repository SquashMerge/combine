import { createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { createTauriSpawnPort } from './ipc/spawn';
import { createTauriClientStatePort, createTauriHostsPort, createTauriLayoutPort } from './ipc/tauri';
import { App } from './shell/App';

const root = document.getElementById('root');
if (root === null) throw new Error('missing #root');
createRoot(root).render(
  createElement(App, {
    port: createTauriHostsPort(),
    clientStatePort: createTauriClientStatePort(),
    layoutPort: createTauriLayoutPort(),
    spawnPort: createTauriSpawnPort(),
  }),
);
