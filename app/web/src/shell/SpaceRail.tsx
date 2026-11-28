import { useState } from 'react';
import { type HostView, type HostsSnapshot, isConnected } from '../model/hosts';
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
  const [retryError, setRetryError] = useState<string | null>(null);
  const connected = isConnected(host);
  const dimmed = connected ? 'false' : 'true';
  const retry = (): void => {
    setRetryError(null);
    port.retryHost(host.name).catch((cause: unknown) => {
      setRetryError(cause instanceof Error ? cause.message : String(cause));
    });
  };
  const detail =
    connection.kind === 'dropped' || connection.kind === 'failed' ? connection.detail : null;
  return (
    <section aria-label={host.name} data-connection={connection.kind} data-dimmed={dimmed}>
      <h2>{host.name}</h2>
      <p role="status">{describeConnection(connection)}</p>
      {detail !== null && <p className="detail">{detail}</p>}
      {connection.kind === 'failed' && (
        <>
          <button type="button" onClick={retry}>
            Retry
          </button>
          {retryError !== null && (
            <p className="detail" role="alert">
              Retry failed: {retryError}
            </p>
          )}
        </>
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
      {connected && host.spaces.length === 0 && <p>No spaces</p>}
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
            Fix <code>{config.path}</code> and restart Combine. Combine has not changed the file.
          </p>
        </>
      ) : hosts.length === 0 ? (
        <p className="config-hint">No hosts yet. Add them to <code>{config.path}</code>.</p>
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
