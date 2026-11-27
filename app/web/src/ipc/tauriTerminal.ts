import { Channel, invoke as tauriInvoke } from '@tauri-apps/api/core';
import { ModelError } from '../model/ids';
import { parseDataFrame, parseTerminalStreamEvent } from '../model/terminal';
import {
  TERMINAL_ACK_COMMAND,
  TERMINAL_ATTACH_COMMAND,
  TERMINAL_AUTOMATIC_REPLY_COMMAND,
  TERMINAL_CLOSE_STREAM_COMMAND,
  TERMINAL_DETACH_COMMAND,
  TERMINAL_FOCUS_COMMAND,
  TERMINAL_INPUT_COMMAND,
  TERMINAL_OPEN_STREAM_COMMAND,
  TERMINAL_REQUEST_OWNERSHIP_COMMAND,
  TERMINAL_RESIZE_COMMAND,
  TERMINAL_RESUME_COMMAND,
  TERMINAL_SET_DISPLAY_COMMAND,
} from './names';
import type { TerminalPort, TerminalStream, TerminalStreamListener } from './terminalPort';

async function invoke<T = unknown>(
  command: string,
  ...rest: [args?: Parameters<typeof tauriInvoke>[1], options?: Parameters<typeof tauriInvoke>[2]]
): Promise<T> {
  try {
    return await tauriInvoke<T>(command, ...rest);
  } catch (error) {
    if (error instanceof Error) throw error;
    throw new Error(typeof error === 'string' ? error : JSON.stringify(error));
  }
}

function decodeMessage(message: unknown) {
  if (message instanceof ArrayBuffer) return parseDataFrame(new Uint8Array(message));
  if (ArrayBuffer.isView(message)) {
    return parseDataFrame(new Uint8Array(message.buffer, message.byteOffset, message.byteLength));
  }
  return parseTerminalStreamEvent(message);
}

function createStream(stream: string): TerminalStream {
  return {
    async attach(terminal, options, instance) {
      await invoke(TERMINAL_ATTACH_COMMAND, { stream, terminal, options, instance });
    },
    async resume(cursor, instance) {
      await invoke(TERMINAL_RESUME_COMMAND, { stream, cursor, instance });
    },
    async detach(attachment) {
      await invoke(TERMINAL_DETACH_COMMAND, { stream, attachment });
    },
    async ack(cursor) {
      await invoke(TERMINAL_ACK_COMMAND, { stream, cursor });
    },
    async input(attachment, bytes) {
      await invoke(TERMINAL_INPUT_COMMAND, bytes, {
        headers: { 'Combine-Stream': stream, 'Combine-Attachment': attachment },
      });
    },
    async automaticReply(attachment, epoch, ownershipGeneration, bytes) {
      await invoke(TERMINAL_AUTOMATIC_REPLY_COMMAND, bytes, {
        headers: {
          'Combine-Stream': stream,
          'Combine-Attachment': attachment,
          'Combine-Epoch': epoch,
          'Combine-Ownership-Generation': ownershipGeneration,
        },
      });
    },
    async resize(attachment, size, ownershipGeneration) {
      await invoke(TERMINAL_RESIZE_COMMAND, {
        stream,
        attachment,
        cols: size.cols,
        rows: size.rows,
        ownershipGeneration,
      });
    },
    async focus(attachment) {
      await invoke(TERMINAL_FOCUS_COMMAND, { stream, attachment });
    },
    async setDisplay(attachment, display) {
      await invoke(TERMINAL_SET_DISPLAY_COMMAND, { stream, attachment, display });
    },
    async requestOwnership(attachment, ifVacant) {
      await invoke(TERMINAL_REQUEST_OWNERSHIP_COMMAND, { stream, attachment, ifVacant });
    },
    async close() {
      await invoke(TERMINAL_CLOSE_STREAM_COMMAND, { stream });
    },
  };
}

export function createTauriTerminalPort(): TerminalPort {
  return {
    async openStream(hostId, listener: TerminalStreamListener) {
      const channel = new Channel<unknown>();
      channel.onmessage = (message) => {
        let event;
        try {
          event = decodeMessage(message);
        } catch (error) {
          if (error instanceof ModelError) {
            console.error(error);
            return;
          }
          throw error;
        }
        try {
          listener(event);
        } catch (error) {
          // A throw here would leave the Channel's message index behind and stall every later message.
          console.error(error);
        }
      };
      const stream = await invoke<string>(TERMINAL_OPEN_STREAM_COMMAND, { hostId, channel });
      return createStream(stream);
    },
  };
}
