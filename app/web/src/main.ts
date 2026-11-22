import { createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { createTauriHostsPort } from './ipc/tauri';
import { App } from './shell/App';

const root = document.getElementById('root');
if (root === null) throw new Error('missing #root');
createRoot(root).render(createElement(App, { port: createTauriHostsPort() }));
