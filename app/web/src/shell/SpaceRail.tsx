import type { HostView, HostsSnapshot } from '../model/hosts';
import { type SpaceKey, folderName, sameSpace, spaceKeyId } from '../model/space';
import type { HostsPort } from '../ipc/port';
import { describeConnection } from './connectionText';
import { sortSpaces } from './order';

interface SpaceRailProps {
  snapshot: HostsSnapshot;
  selected: SpaceKey | null;
  port: HostsPort;
  readOnlySpaces?: ReadonlySet<string>;
  onSelect: (key: SpaceKey) => void;
}

interface HostSectionProps {
  host: HostView;
  selected: SpaceKey | null;
  port: HostsPort;
  readOnlySpaces: ReadonlySet<string>;
  onSelect: (key: SpaceKey) => void;
}

function HostSection({ host, selected, port, readOnlySpaces, onSelect }: HostSectionProps) {
  const { connection } = host;
  const dimmed = connection.kind !== 'connected' ? 'true' : 'false';
  const detail =
    connection.kind === 'dropped' || connection.kind === 'failed' ? connection.detail : null;
  return (
    <section aria-label={host.name} data-connection={connection.kind} data-dimmed={dimmed}>
      <h2>{host.name}</h2>
      <p role="status">{describeConnection(connection)}</p>
      {detail !== null && <p className="detail">{detail}</p>}
      {connection.kind === 'failed' && (
        <button type="button" onClick={() => void port.retryHost(host.name)}>
          Retry
        </button>
      )}
      <ul>
        {sortSpaces(host.spaces).map((space) => {
          const isSelected = selected !== null && sameSpace(selected, space.key);
          const readOnly = readOnlySpaces.has(spaceKeyId(space.key));
          return (
            <li key={spaceKeyId(space.key)}>
              <button
                type="button"
                title={space.key.path}
                data-dimmed={dimmed}
                data-read-only={readOnly ? 'true' : undefined}
                aria-current={isSelected ? 'true' : undefined}
                onClick={() => onSelect(space.key)}
              >
                {folderName(space.key.path)}
                {readOnly && <span className="read-only">read-only</span>}
              </button>
            </li>
          );
        })}
      </ul>
      {connection.kind === 'connected' && host.spaces.length === 0 && <p>No spaces</p>}
    </section>
  );
}

const NO_SPACES: ReadonlySet<string> = new Set();

export function SpaceRail({
  snapshot,
  selected,
  port,
  readOnlySpaces = NO_SPACES,
  onSelect,
}: SpaceRailProps) {
  const { config, hosts } = snapshot;
  return (
    <nav aria-label="Spaces">
      {config.error !== null ? (
        <>
          <p role="alert">{config.error}</p>
          <p className="config-hint">
            Fix {config.path} and restart Combine. Combine has not changed the file.
          </p>
        </>
      ) : hosts.length === 0 ? (
        <p className="config-hint">No hosts yet. Add them to {config.path}.</p>
      ) : (
        hosts.map((host) => (
          <HostSection
            key={host.name}
            host={host}
            selected={selected}
            port={port}
            readOnlySpaces={readOnlySpaces}
            onSelect={onSelect}
          />
        ))
      )}
    </nav>
  );
}
