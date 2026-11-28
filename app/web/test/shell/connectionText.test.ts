import { describe, expect, it } from 'vitest';
import { connectionShape, describeConnection } from '../../src/shell/connectionText';

describe('describeConnection', () => {
  it('connecting', () => {
    expect(describeConnection({ kind: 'connecting' })).toBe('Connecting…');
  });

  it('connected', () => {
    expect(describeConnection({ kind: 'connected' })).toBe('Connected');
  });

  it('dropped shows the reason and the retry delay', () => {
    expect(
      describeConnection({ kind: 'dropped', reason: 'connection closed', detail: null, retryInMs: 4000 }),
    ).toBe('Dropped: connection closed. Retrying in 4 s.');
  });

  it('dropped strips one trailing full stop and rounds the delay up', () => {
    expect(
      describeConnection({
        kind: 'dropped',
        reason: 'Connection closed by remote host.',
        detail: null,
        retryInMs: 1200,
      }),
    ).toBe('Dropped: Connection closed by remote host. Retrying in 2 s.');
  });

  it('reconnecting shows the attempt', () => {
    expect(describeConnection({ kind: 'reconnecting', attempt: 2 })).toBe(
      'Reconnecting (attempt 2)…',
    );
  });

  it('failed shows its message', () => {
    expect(describeConnection({ kind: 'failed', message: 'it broke', detail: 'why' })).toBe('it broke');
  });
});

describe('connectionShape', () => {
  it('draws one shape per connection kind', () => {
    expect(connectionShape('connected')).toBe('check');
    expect(connectionShape('connecting')).toBe('half-ring');
    expect(connectionShape('reconnecting')).toBe('half-ring');
    expect(connectionShape('dropped')).toBe('ring');
    expect(connectionShape('failed')).toBe('cross');
  });
});
