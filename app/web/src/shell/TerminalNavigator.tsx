import type { ReactNode } from 'react';
import { type HostView, type HostsSnapshot, type SpaceView, isConnected } from '../model/hosts';
import type { TermId } from '../model/ids';
import { type SpaceKey, folderName, sameSpace, spaceKeyId } from '../model/space';
import { StateGlyph, terminalStateShape } from '../panels/StateGlyph';
import { groupAllTerminals } from './order';

export type ActiveTerminals = ReadonlyMap<string, TermId>;

interface TerminalNavigatorProps {
  snapshot: HostsSnapshot;
  selected: SpaceKey | null;
  active: ActiveTerminals;
  onSelectTerminal: (space: SpaceKey, id: TermId) => void;
  allSpaces: boolean;
  onToggleAllSpaces: () => void;
  actions?: ReactNode;
}

interface TerminalListProps {
  host: HostView;
  space: SpaceView;
  active: ActiveTerminals;
  onSelectTerminal: (space: SpaceKey, id: TermId) => void;
}

function TerminalList({ host, space, active, onSelectTerminal }: TerminalListProps) {
  const dimmed = isConnected(host) ? 'false' : 'true';
  return (
    <ul>
      {space.terminals.map((terminal) => {
        const isActive = active.get(spaceKeyId(space.key)) === terminal.id;
        return (
          <li
            key={terminal.id}
            data-term-id={terminal.id}
            data-dimmed={dimmed}
            aria-current={isActive ? 'true' : undefined}
          >
            <button
              type="button"
              className="terminal-entry"
              aria-label={`${terminal.name}, ${terminal.command}, ${terminal.state}`}
              onClick={() => onSelectTerminal(space.key, terminal.id)}
            >
              <StateGlyph shape={terminalStateShape(terminal.state)} />
              <span className="name">{terminal.name}</span>
              <span className="command">{terminal.command}</span>
              <span className="state">{terminal.state}</span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

interface ListingProps {
  snapshot: HostsSnapshot;
  active: ActiveTerminals;
  onSelectTerminal: (space: SpaceKey, id: TermId) => void;
}

function AllSpaces({ snapshot, active, onSelectTerminal }: ListingProps) {
  const groups = groupAllTerminals(snapshot);
  if (groups.length === 0) return <p>No terminals.</p>;
  return (
    <>
      {groups.map(({ host, spaces }) => (
        <section key={host.name} aria-label={host.name}>
          {spaces.map((space) => (
            <section key={spaceKeyId(space.key)} aria-label={folderName(space.key.path)}>
              <TerminalList
                host={host}
                space={space}
                active={active}
                onSelectTerminal={onSelectTerminal}
              />
            </section>
          ))}
        </section>
      ))}
    </>
  );
}

function SelectedSpace({
  snapshot,
  selected,
  active,
  onSelectTerminal,
}: ListingProps & { selected: SpaceKey | null }) {
  if (selected !== null) {
    for (const host of snapshot.hosts) {
      const space = host.spaces.find((candidate) => sameSpace(candidate.key, selected));
      if (space === undefined) continue;
      if (space.terminals.length === 0) return <p>No terminals in this space.</p>;
      return (
        <TerminalList host={host} space={space} active={active} onSelectTerminal={onSelectTerminal} />
      );
    }
  }
  return <p>Select a space to see its terminals.</p>;
}

export function TerminalNavigator({
  snapshot,
  selected,
  active,
  onSelectTerminal,
  allSpaces,
  onToggleAllSpaces,
  actions,
}: TerminalNavigatorProps) {
  return (
    <section aria-label="Terminals">
      <button type="button" aria-pressed={allSpaces} onClick={onToggleAllSpaces}>
        All spaces
      </button>
      {actions}
      {allSpaces ? (
        <AllSpaces snapshot={snapshot} active={active} onSelectTerminal={onSelectTerminal} />
      ) : (
        <SelectedSpace
          snapshot={snapshot}
          selected={selected}
          active={active}
          onSelectTerminal={onSelectTerminal}
        />
      )}
    </section>
  );
}
