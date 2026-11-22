import type { HostView, HostsSnapshot, TerminalSummary } from '../model/hosts';
import { type SpaceKey, folderName, sameSpace, spaceKeyId } from '../model/space';
import { groupAllTerminals } from './order';

interface TerminalNavigatorProps {
  snapshot: HostsSnapshot;
  selected: SpaceKey | null;
  allSpaces: boolean;
  onToggleAllSpaces: () => void;
}

function TerminalList({ host, terminals }: { host: HostView; terminals: TerminalSummary[] }) {
  const dimmed = host.connection.kind !== 'connected' ? 'true' : 'false';
  return (
    <ul>
      {terminals.map((terminal) => (
        <li key={terminal.id} data-term-id={terminal.id} data-dimmed={dimmed}>
          <span className="name">{terminal.name}</span>
          <span className="command">{terminal.command}</span>
          <span className="state">{terminal.state}</span>
        </li>
      ))}
    </ul>
  );
}

function AllSpaces({ snapshot }: { snapshot: HostsSnapshot }) {
  const groups = groupAllTerminals(snapshot);
  if (groups.length === 0) return <p>No terminals.</p>;
  return (
    <>
      {groups.map(({ host, spaces }) => (
        <section key={host.name} aria-label={host.name}>
          {spaces.map((space) => (
            <section key={spaceKeyId(space.key)} aria-label={folderName(space.key.path)}>
              <TerminalList host={host} terminals={space.terminals} />
            </section>
          ))}
        </section>
      ))}
    </>
  );
}

function SelectedSpace({ snapshot, selected }: { snapshot: HostsSnapshot; selected: SpaceKey | null }) {
  if (selected !== null) {
    for (const host of snapshot.hosts) {
      const space = host.spaces.find((candidate) => sameSpace(candidate.key, selected));
      if (space === undefined) continue;
      if (space.terminals.length === 0) return <p>No terminals in this space.</p>;
      return <TerminalList host={host} terminals={space.terminals} />;
    }
  }
  return <p>Select a space to see its terminals.</p>;
}

export function TerminalNavigator({
  snapshot,
  selected,
  allSpaces,
  onToggleAllSpaces,
}: TerminalNavigatorProps) {
  return (
    <section aria-label="Terminals">
      <button type="button" aria-pressed={allSpaces} onClick={onToggleAllSpaces}>
        All spaces
      </button>
      {allSpaces ? (
        <AllSpaces snapshot={snapshot} />
      ) : (
        <SelectedSpace snapshot={snapshot} selected={selected} />
      )}
    </section>
  );
}
