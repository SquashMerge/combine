import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import type { HostsSnapshot } from '../model/hosts';
import { fuzzyFilter } from './fuzzy';
import {
  type PaletteHandlers,
  type PalettePanels,
  buildPaletteEntries,   runPaletteAction,
} from './paletteEntries';
import { useGlobalShortcut } from './useGlobalShortcut';

interface CommandPaletteProps extends PaletteHandlers {
  snapshot: HostsSnapshot;
  panels?: PalettePanels | null;
}

interface PaletteDialogProps extends CommandPaletteProps {
  onClose: () => void;
}

function PaletteDialog({ snapshot, panels = null, onClose, ...handlers }: PaletteDialogProps) {
  const [query, setQuery] = useState('');
  const [index, setIndex] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const entries = useMemo(() => buildPaletteEntries(snapshot, panels), [snapshot, panels]);
  const results = useMemo(() => fuzzyFilter(entries, query), [entries, query]);
  const active = Math.min(index, Math.max(results.length - 1, 0));

  useEffect(() => {
    const previous = document.activeElement;
    input.current?.focus();
    return () => {
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);

  function onKeyDown(event: KeyboardEvent) {
    switch (event.key) {
      case 'Escape':
        event.preventDefault();
        onClose();
        return;
      case 'ArrowDown':
        event.preventDefault();
        setIndex(Math.min(active + 1, results.length - 1));
        return;
      case 'ArrowUp':
        event.preventDefault();
        setIndex(Math.max(active - 1, 0));
        return;
      case 'Tab':
        event.preventDefault();
        input.current?.focus();
        return;
      case 'Enter': {
        event.preventDefault();
        const entry = results[active];
        if (entry === undefined) return;
        runPaletteAction(entry.action, handlers);
        onClose();
        return;
      }
    }
  }

  const optionId = (id: string) => `palette-option-${id}`;
  const activeEntry = results[active];

  return (
    <div
      className="palette-backdrop"
      onKeyDown={onKeyDown}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="palette" role="dialog" aria-modal="true" aria-label="Command palette">
        <input
          ref={input}
          type="text"
          role="combobox"
          aria-label="Search spaces, terminals and panels"
          aria-autocomplete="list"
          aria-expanded="true"
          aria-controls="palette-results"
          aria-activedescendant={activeEntry === undefined ? undefined : optionId(activeEntry.id)}
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setIndex(0);
          }}
        />
        <ul id="palette-results" role="listbox" aria-label="Results">
          {results.map((entry, position) => (
            <li
              key={entry.id}
              id={optionId(entry.id)}
              role="option"
              aria-selected={position === active}
              data-palette-entry={entry.action.kind}
              onMouseMove={() => setIndex(position)}
              onClick={() => {
                runPaletteAction(entry.action, handlers);
                onClose();
              }}
            >
              <span className="label">{entry.label}</span>
              <span className="detail">{entry.detail}</span>
            </li>
          ))}
        </ul>
        {results.length === 0 && <p className="palette-empty">No matches.</p>}
      </div>
    </div>
  );
}

export function CommandPalette(props: CommandPaletteProps) {
  const [open, setOpen] = useState(false);
  const toggle = useCallback(() => setOpen((on) => !on), []);
  const close = useCallback(() => setOpen(false), []);
  useGlobalShortcut('k', toggle);
  if (!open) return null;
  return <PaletteDialog {...props} onClose={close} />;
}
