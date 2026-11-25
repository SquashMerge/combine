import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, it } from 'vitest';
import {
  CLIENT_STATE_COMMAND,
  CSP_PROBE_REPORT_COMMAND,
  GET_PANEL_COMMAND,
  HOSTS_EVENT,
  HOSTS_SNAPSHOT_COMMAND,
  LAYOUT_EVENT,
  MUTATE_LAYOUT_COMMAND,
  OPEN_URL_COMMAND,
  PANELS_EVENT,
  REPORT_RENDER_COMMAND,
  RETRY_HOST_COMMAND,
  SET_CLIENT_SPACE_STATE_COMMAND,
  SET_LAST_SPACE_COMMAND,
  TERMINAL_OPEN_STREAM_COMMAND,
  TERMINAL_CLOSE_STREAM_COMMAND,
  TERMINAL_ATTACH_COMMAND,
  TERMINAL_RESUME_COMMAND,
  TERMINAL_DETACH_COMMAND,
  TERMINAL_ACK_COMMAND,
  TERMINAL_RESIZE_COMMAND,
  TERMINAL_FOCUS_COMMAND,
  TERMINAL_SET_DISPLAY_COMMAND,
  TERMINAL_REQUEST_OWNERSHIP_COMMAND,
  TERMINAL_INPUT_COMMAND,
  TERMINAL_AUTOMATIC_REPLY_COMMAND,
  UNWATCH_LAYOUT_COMMAND,
  WATCH_LAYOUT_COMMAND,
} from '../../src/ipc/names';

const contract = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'ipc-contract.json'), 'utf8'),
) as {
  hostsEvent: string;
  layoutEvent: string;
  panelsEvent: string;
  commands: {
    snapshot: string;
    retry: string;
    clientState: string;
    setClientSpaceState: string;
    setLastSpace: string;
    watchLayout: string;
    unwatchLayout: string;
    mutateLayout: string;
    terminalOpenStream: string;
    terminalCloseStream: string;
    terminalAttach: string;
    terminalResume: string;
    terminalDetach: string;
    terminalAck: string;
    terminalResize: string;
    terminalFocus: string;
    terminalSetDisplay: string;
    terminalRequestOwnership: string;
    terminalInput: string;
    terminalAutomaticReply: string;
    openUrl: string;
    getPanel: string;
    reportRender: string;
    cspProbeReport: string;
  };
};

it('matches the shared ipc contract fixture', () => {
  expect(HOSTS_EVENT).toBe(contract.hostsEvent);
  expect(HOSTS_SNAPSHOT_COMMAND).toBe(contract.commands.snapshot);
  expect(RETRY_HOST_COMMAND).toBe(contract.commands.retry);
  expect(CLIENT_STATE_COMMAND).toBe(contract.commands.clientState);
  expect(SET_CLIENT_SPACE_STATE_COMMAND).toBe(contract.commands.setClientSpaceState);
  expect(SET_LAST_SPACE_COMMAND).toBe(contract.commands.setLastSpace);
  expect(LAYOUT_EVENT).toBe(contract.layoutEvent);
  expect(WATCH_LAYOUT_COMMAND).toBe(contract.commands.watchLayout);
  expect(UNWATCH_LAYOUT_COMMAND).toBe(contract.commands.unwatchLayout);
  expect(MUTATE_LAYOUT_COMMAND).toBe(contract.commands.mutateLayout);
  expect(TERMINAL_OPEN_STREAM_COMMAND).toBe(contract.commands.terminalOpenStream);
  expect(TERMINAL_CLOSE_STREAM_COMMAND).toBe(contract.commands.terminalCloseStream);
  expect(TERMINAL_ATTACH_COMMAND).toBe(contract.commands.terminalAttach);
  expect(TERMINAL_RESUME_COMMAND).toBe(contract.commands.terminalResume);
  expect(TERMINAL_DETACH_COMMAND).toBe(contract.commands.terminalDetach);
  expect(TERMINAL_ACK_COMMAND).toBe(contract.commands.terminalAck);
  expect(TERMINAL_RESIZE_COMMAND).toBe(contract.commands.terminalResize);
  expect(TERMINAL_FOCUS_COMMAND).toBe(contract.commands.terminalFocus);
  expect(TERMINAL_SET_DISPLAY_COMMAND).toBe(contract.commands.terminalSetDisplay);
  expect(TERMINAL_REQUEST_OWNERSHIP_COMMAND).toBe(contract.commands.terminalRequestOwnership);
  expect(TERMINAL_INPUT_COMMAND).toBe(contract.commands.terminalInput);
  expect(TERMINAL_AUTOMATIC_REPLY_COMMAND).toBe(contract.commands.terminalAutomaticReply);
  expect(OPEN_URL_COMMAND).toBe(contract.commands.openUrl);
  expect(PANELS_EVENT).toBe(contract.panelsEvent);
  expect(GET_PANEL_COMMAND).toBe(contract.commands.getPanel);
  expect(REPORT_RENDER_COMMAND).toBe(contract.commands.reportRender);
  expect(CSP_PROBE_REPORT_COMMAND).toBe(contract.commands.cspProbeReport);
});
