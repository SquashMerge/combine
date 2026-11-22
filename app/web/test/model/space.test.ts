import { describe, expect, it } from 'vitest';
import { parseHostId } from '../../src/model/ids';
import { folderName, sameSpace, spaceKeyId } from '../../src/model/space';

const a = parseHostId('0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c');
const b = parseHostId('6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c');

describe('spaceKeyId', () => {
  it('joins host id and path', () => {
    expect(spaceKeyId({ hostId: a, path: '/src/x' })).toBe(`${a}:/src/x`);
  });

  it('differs for two hosts with the same path', () => {
    expect(spaceKeyId({ hostId: a, path: '/src/x' })).not.toBe(
      spaceKeyId({ hostId: b, path: '/src/x' }),
    );
  });
});

describe('sameSpace', () => {
  it('requires both host and path to match', () => {
    expect(sameSpace({ hostId: a, path: '/p' }, { hostId: a, path: '/p' })).toBe(true);
    expect(sameSpace({ hostId: a, path: '/p' }, { hostId: b, path: '/p' })).toBe(false);
    expect(sameSpace({ hostId: a, path: '/p' }, { hostId: a, path: '/q' })).toBe(false);
  });
});

describe('folderName', () => {
  it.each([
    ['/Users/x/src/trellis', 'trellis'],
    ['/Users/x/src/trellis/', 'trellis'],
    ['trellis', 'trellis'],
    ['/', '/'],
    ['', '/'],
    ['//', '/'],
  ])('%j -> %j', (path, expected) => {
    expect(folderName(path)).toBe(expected);
  });
});
