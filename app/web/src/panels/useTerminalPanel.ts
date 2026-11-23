import { useCallback, useEffect, useRef, useSyncExternalStore } from 'react';
import type { HostId, TermId } from '../model/ids';
import type { TerminalPort } from '../ipc/terminalPort';
import type { XtermAdapterFactory } from '../renderers/terminal/adapter';
import type { PanelView } from './attachmentMachine';
import { TerminalController } from './terminalController';

export interface UseTerminalPanelOptions {
  port: TerminalPort;
  hostId: HostId;
  termId: TermId;
  visible: boolean;
  createAdapter: XtermAdapterFactory;
}

export interface TerminalPanelState {
  view: PanelView;
  hostRef: (element: HTMLDivElement | null) => void;
  onContainerSize: (size: { width: number; height: number }) => void;
  takeInput: () => void;
  reattach: () => void;
}

/**
 * `hostId` and `termId` are fixed for the panel's lifetime: key the panel by them. The controller is created once and `stop()` is permanent, so never run this hook under React.StrictMode or a Fast Refresh double-mount.
 */
export function useTerminalPanel({
  port,
  hostId,
  termId,
  visible,
  createAdapter,
}: UseTerminalPanelOptions): TerminalPanelState {
  const controllerRef = useRef<TerminalController | null>(null);
  controllerRef.current ??= new TerminalController({ port, hostId, termId, createAdapter });
  const controller = controllerRef.current;
  const hostElement = useRef<HTMLDivElement | null>(null);

  const hostRef = useCallback((element: HTMLDivElement | null) => {
    hostElement.current = element;
  }, []);

  useEffect(() => {
    const element = hostElement.current;
    if (element === null) return;
    controller.start(element).catch(() => undefined);
    return () => {
      controller.stop().catch(() => undefined);
    };
  }, [controller]);

  useEffect(() => {
    controller.setVisible(visible);
  }, [controller, visible]);

  const view = useSyncExternalStore(
    useCallback((listener) => controller.subscribe(listener), [controller]),
    () => controller.getView(),
  );

  const onContainerSize = useCallback(
    ({ width, height }: { width: number; height: number }) =>
      controller.setContainerSize(width, height),
    [controller],
  );
  const takeInput = useCallback(() => controller.takeInput(), [controller]);
  const reattach = useCallback(() => controller.reattach(), [controller]);

  return { view, hostRef, onContainerSize, takeInput, reattach };
}
