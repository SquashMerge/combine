import type { Cursor, DecimalU64, HostId, TermId } from '../model/ids';
import type {
  AttachRequestOptions,
  Display,
  TerminalSize,
  TerminalStreamEvent,
} from '../model/terminal';

export type TerminalStreamListener = (event: TerminalStreamEvent) => void;

/**
 * One panel's stream to one host. A stream has at most one outstanding `attach` or `resume`.
 * The transport routes that request's reply to this stream and never routes a previous
 * session's reply, so the panel never matches request IDs: they stay inside the transport.
 *
 * Every command rejects with `Error('not connected')` while the host has no live session.
 * Any other rejection is a transport fault.
 */
export interface TerminalStream {
  /**
   * `instance` is the backend instance a pinned panel expects its terminal to belong to; the
   * daemon refuses with `backend_mismatch` when its own differs. `null` for a panel that follows
   * the active terminal.
   */
  attach(terminal: TermId, options: AttachRequestOptions, instance: string | null): Promise<void>;
  resume(cursor: Cursor, instance: string | null): Promise<void>;
  detach(attachment: DecimalU64): Promise<void>;
  ack(cursor: Cursor): Promise<void>;
  input(attachment: DecimalU64, bytes: Uint8Array): Promise<void>;
  automaticReply(
    attachment: DecimalU64,
    epoch: DecimalU64,
    ownershipGeneration: DecimalU64,
    bytes: Uint8Array,
  ): Promise<void>;
  resize(
    attachment: DecimalU64,
    size: TerminalSize,
    ownershipGeneration: DecimalU64,
  ): Promise<void>;
  focus(attachment: DecimalU64): Promise<void>;
  setDisplay(attachment: DecimalU64, display: Display): Promise<void>;
  /**
   * `ifVacant` is the ownership generation under which the panel saw no owner: the daemon grants
   * the request only if the terminal still has no owner under it. `null` always takes ownership.
   */
  requestOwnership(attachment: DecimalU64, ifVacant: DecimalU64 | null): Promise<void>;
  /** Ends the panel stream. */
  close(): Promise<void>;
}

export interface TerminalPort {
  /**
   * Every event for attachments opened through the stream arrives on `listener`, in order.
   * The first event is a `connection` event with the current state. It may arrive before the
   * promise resolves.
   */
  openStream(hostId: HostId, listener: TerminalStreamListener): Promise<TerminalStream>;
}
