import type { ConnectionState } from '../model/hosts';
import type { GlyphShape } from '../panels/StateGlyph';

export function connectionShape(kind: ConnectionState['kind']): GlyphShape {
  switch (kind) {
    case 'connected':
      return 'check';
    case 'connecting':
    case 'reconnecting':
      return 'half-ring';
    case 'dropped':
      return 'ring';
    case 'failed':
      return 'cross';
  }
}

export function describeConnection(state: ConnectionState): string {
  switch (state.kind) {
    case 'connecting':
      return 'Connecting…';
    case 'connected':
      return 'Connected';
    case 'dropped':
      return `Dropped: ${state.reason.replace(/\.$/, '')}. Retrying in ${Math.ceil(state.retryInMs / 1000)} s.`;
    case 'reconnecting':
      return `Reconnecting (attempt ${state.attempt})…`;
    case 'failed':
      return state.message;
  }
}
