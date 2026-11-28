import { useEffect, useRef, useState } from 'react';
import type { SpawnPort } from '../ipc/port';
import { type HostsSnapshot, isConnected } from '../model/hosts';
import type { TermId } from '../model/ids';
import { type SpaceKey, folderName, spaceKeyId } from '../model/space';
import {
  SpawnError,
  type SpawnSource,
  TEMPLATE_CHANGED,
  type TemplateList,
  formatCommandLine,
  quoteWord,
} from '../model/spawn';
import './addTerminal.css';

interface AddTerminalMenuProps {
  port: SpawnPort;
  snapshot: HostsSnapshot;
  selected: SpaceKey | null;
  onSpawned: (space: SpaceKey, terminal: TermId) => void;
}

type Listing =
  | { kind: 'loading' }
  | { kind: 'loaded'; list: TemplateList }
  | { kind: 'failed'; message: string };

const SHELL_KEY = 'shell';

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function AddTerminalMenu({ port, snapshot, selected, onSpawned }: AddTerminalMenuProps) {
  const [open, setOpen] = useState(false);
  const [listing, setListing] = useState<Listing>({ kind: 'loading' });
  const [starting, setStarting] = useState<string | null>(null);
  const [spawnError, setSpawnError] = useState<string | null>(null);
  const generation = useRef(0);
  const busy = useRef(false);
  const selectedId = selected === null ? null : spaceKeyId(selected);

  const host =
    selected === null ? undefined : snapshot.hosts.find((h) => h.hostId === selected.hostId);
  const connected = host !== undefined && isConnected(host);
  const disabled = selected === null || !connected;
  const showMenu = open && !disabled;

  useEffect(() => {
    generation.current += 1;
    busy.current = false;
    setOpen(false);
    setStarting(null);
    setSpawnError(null);
    setListing({ kind: 'loading' });
  }, [selectedId]);

  useEffect(() => {
    if (!showMenu) return;
    function onKeyDown(event: globalThis.KeyboardEvent) {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      close();
    }
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [showMenu]);

  function close() {
    generation.current += 1;
    busy.current = false;
    setOpen(false);
    setStarting(null);
  }

  function loadTemplates(space: SpaceKey) {
    generation.current += 1;
    const mine = generation.current;
    setListing({ kind: 'loading' });
    port.listTemplates(space.hostId, space.path).then(
      (list) => {
        if (generation.current === mine) setListing({ kind: 'loaded', list });
      },
      (error: unknown) => {
        if (generation.current === mine) setListing({ kind: 'failed', message: messageOf(error) });
      },
    );
  }

  function toggle() {
    if (open) {
      close();
      return;
    }
    if (selected === null) return;
    setOpen(true);
    setSpawnError(null);
    loadTemplates(selected);
  }

  function choose(key: string, source: SpawnSource) {
    if (selected === null || busy.current) return;
    busy.current = true;
    const mine = generation.current;
    setStarting(key);
    setSpawnError(null);
    port.spawnTerminal(selected.hostId, selected.path, source).then(
      (terminal) => {
        if (generation.current !== mine) return;
        busy.current = false;
        setStarting(null);
        setOpen(false);
        onSpawned(selected, terminal);
      },
      (error: unknown) => {
        if (generation.current !== mine) return;
        busy.current = false;
        setStarting(null);
        if (error instanceof SpawnError && error.code === TEMPLATE_CHANGED) {
          // The listed definition is stale: show the new one and wait for another click.
          setSpawnError(error.message);
          loadTemplates(selected);
          return;
        }
        setSpawnError(`Couldn't add the terminal: ${messageOf(error)}`);
      },
    );
  }

  const title =
    disabled && selected !== null && host !== undefined ? `${host.name} isn't connected` : undefined;
  const list = listing.kind === 'loaded' ? listing.list : null;
  const itemLabel = (key: string, label: string) => (starting === key ? 'Starting…' : label);

  return (
    <div className="add-terminal">
      <button
        type="button"
        className="button-primary"
        aria-haspopup="menu"
        aria-expanded={showMenu}
        disabled={disabled}
        title={title}
        onClick={toggle}
      >
        Add terminal
      </button>
      {showMenu && selected !== null && (
        <div role="menu" aria-label="Add terminal">
          <button
            type="button"
            role="menuitem"
            disabled={starting !== null}
            onClick={() => {
              choose(SHELL_KEY, { kind: 'shell' });
            }}
          >
            <span>{itemLabel(SHELL_KEY, 'Shell')}</span>
            <span className="detail"> Shell in <code>{folderName(selected.path)}</code></span>
          </button>
          {listing.kind === 'loading' && <p role="status">Loading templates…</p>}
          {list?.templates.map((template) => {
            const key = `template:${template.panel}`;
            return (
              <button
                key={key}
                type="button"
                role="menuitem"
                disabled={starting !== null}
                onClick={() => {
                  choose(key, { kind: 'template', panel: template.panel, hash: template.hash });
                }}
              >
                <span>{itemLabel(key, template.title ?? template.slot)}</span>
                <code className="command">{formatCommandLine(template)}</code>
                {template.cwd !== '' && (
                  <span className="detail">
                    in <code>{quoteWord(template.cwd)}</code>
                  </span>
                )}
              </button>
            );
          })}
          {list?.skipped.map((skipped) => (
            <div
              key={`skipped:${skipped.panel}`}
              role="menuitem"
              aria-disabled="true"
              data-dimmed="true"
            >
              <code>{skipped.panel}</code>: {skipped.reason}
            </div>
          ))}
          {list?.error != null && <p role="alert">{list.error}</p>}
          {listing.kind === 'failed' && (
            <p role="alert">Couldn't list templates: {listing.message}</p>
          )}
          {spawnError !== null && <p role="alert">{spawnError}</p>}
        </div>
      )}
    </div>
  );
}
