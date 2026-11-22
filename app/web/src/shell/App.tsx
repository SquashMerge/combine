import { useState } from 'react';
import type { SpaceKey } from '../model/space';
import type { HostsPort } from '../ipc/port';
import { SpaceRail } from './SpaceRail';
import { TerminalNavigator } from './TerminalNavigator';
import { useHostsSnapshot } from './useHostsSnapshot';
import './shell.css';

export function App({ port }: { port: HostsPort }) {
  const { snapshot, error } = useHostsSnapshot(port);
  const [selected, setSelected] = useState<SpaceKey | null>(null);
  const [allSpaces, setAllSpaces] = useState(false);

  if (error !== null) return <p role="alert">Cannot reach the Combine backend: {error}</p>;
  if (snapshot === null) return null;

  return (
    <div className="shell">
      <SpaceRail snapshot={snapshot} selected={selected} port={port} onSelect={setSelected} />
      <TerminalNavigator
        snapshot={snapshot}
        selected={selected}
        allSpaces={allSpaces}
        onToggleAllSpaces={() => setAllSpaces((on) => !on)}
      />
    </div>
  );
}
