import { describe, expect, it } from 'vitest';
import type { TermId } from '../../src/model/ids';
import { emptySpaceState } from '../../src/model/clientState';
import { findSpace, pickAutoOpenTerminal } from '../../src/model/autoOpen';
import { HOST_A, HOST_B, host, snapshot, space, terminal } from '../support/snapshots';

const stored = (active: string | null) => ({
  ...emptySpaceState('/w'),
  activeTerminal: active as TermId | null,
});

describe('pickAutoOpenTerminal', () => {
  it('picks_first_listed_not_sorted', () => {
    const terminals = [terminal('hc:t9', 'z'), terminal('hc:t1', 'a')];
    expect(pickAutoOpenTerminal(undefined, terminals)).toBe('hc:t9');
    expect(pickAutoOpenTerminal(stored(null), terminals)).toBe('hc:t9');
  });

  it('skips_exited_terminals', () => {
    const terminals = [terminal('hc:t1', 'a', 'a', 'exited'), terminal('hc:t2', 'b', 'b', 'running')];
    expect(pickAutoOpenTerminal(undefined, terminals)).toBe('hc:t2');
  });

  it('keeps_valid_stored_terminal_even_if_exited', () => {
    const terminals = [terminal('hc:t1', 'a'), terminal('hc:t2', 'b', 'b', 'exited')];
    expect(pickAutoOpenTerminal(stored('hc:t2'), terminals)).toBeNull();
    expect(pickAutoOpenTerminal(stored('hc:t1'), terminals)).toBeNull();
  });

  it('replaces_stored_terminal_that_is_gone', () => {
    const terminals = [terminal('hc:t1', 'a')];
    expect(pickAutoOpenTerminal(stored('hc:t7'), terminals)).toBe('hc:t1');
  });

  it('all_exited_or_empty_gives_null', () => {
    expect(pickAutoOpenTerminal(undefined, [])).toBeNull();
    expect(
      pickAutoOpenTerminal(stored('hc:t7'), [terminal('hc:t1', 'a', 'a', 'exited')]),
    ).toBeNull();
  });
});

describe('findSpace', () => {
  it('finds_a_space_by_host_and_path', () => {
    const snap = snapshot([
      host('a', HOST_A, { kind: 'connected' }, [space(HOST_A, '/w')]),
      host('b', HOST_B, { kind: 'connected' }, [space(HOST_B, '/w', [terminal('hc:t1', 'a')])]),
    ]);
    expect(findSpace(snap, { hostId: HOST_B, path: '/w' })?.terminals).toHaveLength(1);
    expect(findSpace(snap, { hostId: HOST_A, path: '/x' })).toBeUndefined();
  });
});
