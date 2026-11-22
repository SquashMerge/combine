import type { HostId } from './ids';

export interface SpaceKey {
  hostId: HostId;
  path: string;
}

export function spaceKeyId(key: SpaceKey): string {
  return `${key.hostId}:${key.path}`;
}

export function sameSpace(a: SpaceKey, b: SpaceKey): boolean {
  return a.hostId === b.hostId && a.path === b.path;
}

export function folderName(path: string): string {
  const segments = path.split('/').filter((segment) => segment !== '');
  return segments[segments.length - 1] ?? '/';
}
