import { useCallback, useState } from 'react';
import type { TermId } from '../model/ids';
import type { SpaceKey } from '../model/space';
import type { HostsPort } from '../ipc/port';
import { CommandPalette } from './CommandPalette';
import { SpaceRail } from './SpaceRail';
import { type ActiveTerminal, TerminalNavigator } from './TerminalNavigator';
import { useHostsSnapshot } from './useHostsSnapshot';
import './shell.css';

export function App({ port }: { port: HostsPort }) {
  const { snapshot, error } = useHostsSnapshot(port);
  const [selected, setSelected] = useState<SpaceKey | null>(null);
  const [active, setActive] = useState<ActiveTerminal | null>(null);
  const [allSpaces, setAllSpaces] = useState(false);

  const selectSpace = useCallback((space: SpaceKey) => setSelected(space), []);
  const selectTerminal = useCallback((space: SpaceKey, id: TermId) => {
    setSelected(space);
    setActive({ hostId: space.hostId, id });
  }, []);
  const retry = useCallback((name: string) => void port.retryHost(name), [port]);

  if (error !== null) return <p role="alert">Cannot reach the Combine backend: {error}</p>;
  if (snapshot === null) return null;

  return (
    <div className="shell">
      <SpaceRail snapshot={snapshot} selected={selected} port={port} onSelect={selectSpace} />
      <TerminalNavigator
        snapshot={snapshot}
        selected={selected}
        active={active}
        onSelectTerminal={selectTerminal}
        allSpaces={allSpaces}
        onToggleAllSpaces={() => setAllSpaces((on) => !on)}
      />
      <CommandPalette
        snapshot={snapshot}
        onSelectSpace={selectSpace}
        onSelectTerminal={selectTerminal}
        onRetryHost={retry}
      />
    </div>
  );
}
