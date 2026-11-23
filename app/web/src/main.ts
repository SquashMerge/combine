import { createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { createTauriClientStatePort, createTauriHostsPort, createTauriLayoutPort } from './ipc/tauri';
import { createTauriTerminalPort } from './ipc/tauriTerminal';
import { App } from './shell/App';

const root = document.getElementById('root');
if (root === null) throw new Error('missing #root');
// Not wrapped in React.StrictMode: a terminal panel's controller is created once and stop() is permanent.
createRoot(root).render(
  createElement(App, {
    port: createTauriHostsPort(),
    clientStatePort: createTauriClientStatePort(),
    layoutPort: createTauriLayoutPort(),
    terminalPort: createTauriTerminalPort(),
  }),
);
