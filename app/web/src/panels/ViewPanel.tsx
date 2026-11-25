import { useCallback, useEffect, useRef, useState } from 'react';
import type { OpenUrlPort, PanelPort } from '../ipc/port';
import type { HostId } from '../model/ids';
import type { PanelEntry } from '../model/panels';
import type { RenderOutcome, RendererName } from '../model/render';
import { renderSource } from '../renderers/lazy';
import { attachRenderedLinks } from './renderedLinks';
import {
  type Effect,
  type Input,
  type MachineState,
  decide,
  initialState,
  viewState,
  viewText,
} from './viewPanelMachine';
import './viewPanel.css';

export interface ViewPanelProps {
  hostId: HostId;
  space: string;
  panelId: string;
  focused: boolean;
  entry: PanelEntry;
  session: number;
  connected: boolean;
  port: PanelPort;
  openUrlPort?: OpenUrlPort;
  render?: (renderer: RendererName, source: string) => Promise<RenderOutcome>;
}

export const INERT_OPEN_URL: OpenUrlPort = { openUrl: async () => {} };

/**
 * Fetches the listed revision of one panel, draws it with the renderers and reports what was
 * shown. `hostId`, `space` and `panelId` are fixed for its lifetime: key it by `${hostId}/${space}/${panelId}`.
 */
export function ViewPanel({
  hostId,
  space,
  panelId,
  focused,
  entry,
  session,
  connected,
  port,
  openUrlPort,
  render = renderSource,
}: ViewPanelProps) {
  const [state, setState] = useState<MachineState>(initialState);
  const current = useRef(state);
  const content = useRef<HTMLDivElement>(null);
  // Held in refs so the dispatcher stays one function for the panel's lifetime.
  const live = useRef({ hostId, space, panelId, port, render });
  live.current = { hostId, space, panelId, port, render };

  const dispatch = useCallback((input: Input): void => {
    const decision = decide(current.current, input);
    current.current = decision.state;
    setState(decision.state);
    for (const effect of decision.effects) run(effect);

    function run(effect: Effect): void {
      const { hostId, space, panelId, port, render } = live.current;
      switch (effect.kind) {
        case 'fetch':
          port.fetchPanel(hostId, space, panelId, effect.urgent).then(
            (fetched) => dispatch({ kind: 'fetched', token: effect.token, content: fetched }),
            (cause: unknown) =>
              dispatch({
                kind: 'fetchFailed',
                token: effect.token,
                message: cause instanceof Error ? cause.message : String(cause),
              }),
          );
          return;
        case 'render':
          void render(effect.renderer, effect.source).then((outcome) =>
            dispatch({ kind: 'rendered', token: effect.token, outcome }),
          );
          return;
        case 'report':
          port
            .reportRender(hostId, {
              space,
              panel: panelId,
              revision: effect.shown.revision,
              status: effect.shown.status,
              error: effect.shown.error,
            })
            .then(
              (sent) => dispatch({ kind: 'reportDone', sent }),
              () => dispatch({ kind: 'reportDone', sent: false }),
            );
          return;
        case 'clear':
          content.current?.replaceChildren();
          return;
        case 'show':
          if (effect.fragment === null) content.current?.replaceChildren();
          else content.current?.replaceChildren(effect.fragment);
      }
    }
  }, []);

  useEffect(() => {
    dispatch({ kind: 'sync', entry, session, connected, focused });
  }, [dispatch, entry, session, connected, focused]);

  useEffect(() => () => dispatch({ kind: 'unmount' }), [dispatch]);

  useEffect(() => {
    const element = content.current;
    if (element === null) return undefined;
    return attachRenderedLinks(element, openUrlPort ?? INERT_OPEN_URL);
  }, [openUrlPort]);

  const view = viewState(state, entry);
  const text = viewText(state, entry, panelId);
  return (
    <section
      className="view-panel"
      data-panel-id={panelId}
      data-view-state={view}
      data-shown-revision={state.shown?.revision}
    >
      {text.status !== null && (
        <p className="view-panel-status" role="status">
          {text.status}
        </p>
      )}
      {text.error !== null && (
        <div className="view-panel-error" role="alert">
          {text.error}
          {text.snippet !== null && <pre className="view-panel-snippet">{text.snippet}</pre>}
        </div>
      )}
      <div className="view-panel-content" ref={content} />
    </section>
  );
}
