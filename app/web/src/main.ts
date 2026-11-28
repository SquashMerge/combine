import './shell/fonts';
import './shell/tokens.css';
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
import { renderAfterFonts } from './shell/fontLoad';

const panelPort = withCspProbe(createTauriPanelPort());
const root = document.getElementById('root');
if (root === null) throw new Error('missing #root');
// Not wrapped in React.StrictMode: a terminal panel's controller is created once and stop() is permanent.
// Rendering waits for the bundled fonts so xterm measures its cell in the real face.
void renderAfterFonts(document.fonts, () => {
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
});
