import { createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { withCspProbe } from './ipc/cspProbe';
import { createTauriPanelPort } from './ipc/panels';
import { createTauriSpawnPort } from './ipc/spawn';
import {
  createTauriClientStatePort,
  createTauriHostsPort,
  createTauriLayoutPort,
  createTauriOpenUrlPort,
} from './ipc/tauri';
import { createTauriTerminalPort } from './ipc/tauriTerminal';
import { App } from './shell/App';

const panelPort = withCspProbe(createTauriPanelPort());
const root = document.getElementById('root');
if (root === null) throw new Error('missing #root');
// Not wrapped in React.StrictMode: a terminal panel's controller is created once and stop() is permanent.
createRoot(root).render(
  createElement(App, {
    port: createTauriHostsPort(),
    clientStatePort: createTauriClientStatePort(),
    layoutPort: createTauriLayoutPort(),
    spawnPort: createTauriSpawnPort(),
    terminalPort: createTauriTerminalPort(),
    panelPort,
    openUrlPort: createTauriOpenUrlPort(),
  }),
);
