import type { ClientSpaceState, ClientState } from '../model/clientState';
import type { HostsSnapshot } from '../model/hosts';
import type { DecimalU64, HostId, TermId } from '../model/ids';
import type { LayoutEvent, LayoutOp } from '../model/layout';
import type { PanelContent, PanelsEvent, RenderReport } from '../model/panels';
import type { SpawnSource, TemplateList } from '../model/spawn';

export type Unsubscribe = () => void;

export interface HostsPort {
  getSnapshot(): Promise<HostsSnapshot>;
  onSnapshot(listener: (snapshot: HostsSnapshot) => void): Promise<Unsubscribe>;
  retryHost(name: string): Promise<void>;
}

export interface ClientStatePort {
  load(hostId: HostId): Promise<ClientState>;
  setSpaceState(hostId: HostId, path: string, state: ClientSpaceState): Promise<void>;
  setLastSpace(hostId: HostId, spaceId: string, selectedAtMs: DecimalU64): Promise<void>;
}

export interface LayoutPort {
  watchLayout(hostId: HostId, space: string): Promise<DecimalU64>;
  unwatchLayout(hostId: HostId): Promise<void>;
  mutateLayout(
    hostId: HostId,
    space: string,
    baseLayoutRevision: DecimalU64,
    ops: LayoutOp[],
  ): Promise<DecimalU64>;
  onLayoutEvent(listener: (event: LayoutEvent) => void): Promise<Unsubscribe>;
}

export interface SpawnPort {
  listTemplates(hostId: HostId, space: string): Promise<TemplateList>;
  spawnTerminal(hostId: HostId, space: string, source: SpawnSource): Promise<TermId>;
}

export interface OpenUrlPort {
  /** Asks the shell to open the URL in the system browser. Never rejects; a refusal is a no-op. */
  openUrl(url: string): Promise<void>;
}

export interface PanelPort {
  onPanelsEvent(listener: (event: PanelsEvent) => void): Promise<Unsubscribe>;
  /** Through the host's PanelFetchQueue; rejects with PanelFetchError. */
  fetchPanel(hostId: HostId, space: string, panel: string, urgent: boolean): Promise<PanelContent>;
  /** Closes a panel and resolves when the daemon says it is closed; rejects with PanelFetchError. */
  closePanel(hostId: HostId, space: string, panel: string): Promise<void>;
  /** Keeps a scratch panel in the project and resolves when the daemon says it is kept; rejects with PanelFetchError. */
  pinPanel(hostId: HostId, space: string, panel: string): Promise<void>;
  /** Resolves true when the command was accepted, false otherwise; never rejects. */
  reportRender(hostId: HostId, report: RenderReport): Promise<boolean>;
}
