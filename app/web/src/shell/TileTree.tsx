import { type Dict, dictGet } from '../model/dict';
import type { LayoutNode, LayoutSnapshot, SplitDirection } from '../model/layout';

interface TileTreeProps {
  layout: LayoutSnapshot;
  notice: string | null;
  error: string | null;
  focusedTile: string | null;
  activeTabs: Readonly<Dict<string>>;
  onFocus: (tile: string) => void;
  onSelectTab: (tile: string, panel: string) => void;
  onSplit: (tile: string, dir: SplitDirection) => void;
  onClose: (tile: string) => void;
}

type NodeProps = Omit<TileTreeProps, 'layout' | 'notice' | 'error'> & {
  node: LayoutNode;
  closable: boolean;
};

function Node({ node, closable, ...rest }: NodeProps) {
  if (node.type === 'split') {
    return (
      <div className="split" data-dir={node.dir}>
        <div className="split-pane" style={{ flex: `${node.ratio} 1 0px` }}>
          <Node node={node.a} closable={closable} {...rest} />
        </div>
        <div className="split-pane" style={{ flex: `${1 - node.ratio} 1 0px` }}>
          <Node node={node.b} closable={closable} {...rest} />
        </div>
      </div>
    );
  }
  const { focusedTile, activeTabs, onFocus, onSelectTab, onSplit, onClose } = rest;
  const active = dictGet(activeTabs, node.id) ?? node.active ?? node.panels[0] ?? null;
  const focused = focusedTile === node.id;
  return (
    <div
      className="tile"
      data-tile-id={node.id}
      aria-current={focused ? 'true' : undefined}
      onClick={() => onFocus(node.id)}
    >
      <div className="tile-bar">
        <div role="tablist" aria-label={`Panels of ${node.id}`}>
          {node.panels.map((panel) => (
            <button
              key={panel}
              type="button"
              role="tab"
              data-panel-id={panel}
              aria-selected={panel === active}
              onClick={() => onSelectTab(node.id, panel)}
            >
              {panel}
            </button>
          ))}
        </div>
        <button type="button" onClick={() => onSplit(node.id, 'h')}>
          Split right
        </button>
        <button type="button" onClick={() => onSplit(node.id, 'v')}>
          Split down
        </button>
        <button type="button" disabled={!closable} onClick={() => onClose(node.id)}>
          Close tile
        </button>
      </div>
      <div className="tile-body" role="tabpanel">
        {active ?? 'No panels'}
      </div>
    </div>
  );
}

export function TileTree({ layout, notice, error, ...rest }: TileTreeProps) {
  const dropped = layout.dropped?.message ?? null;
  return (
    <main className="layout" aria-label="Layout" data-read-only={layout.readOnly ? 'true' : undefined}>
      {layout.error !== null && (
        <p className="layout-error" role="alert">
          {layout.error}
        </p>
      )}
      {error !== null && <p role="alert">{error}</p>}
      {dropped !== null && (
        <p className="layout-notice" role="status">
          {dropped}
        </p>
      )}
      {notice !== null && (
        <p className="layout-notice" role="status">
          {notice}
        </p>
      )}
      <Node node={layout.root} closable={layout.root.type === 'split'} {...rest} />
    </main>
  );
}
