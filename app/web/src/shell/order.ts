import type { HostView, HostsSnapshot, SpaceView } from '../model/hosts';
import { folderName } from '../model/space';

export interface HostGroup {
  host: HostView;
  spaces: SpaceView[];
}

function before(a: string, b: string): number {
  if (a < b) return -1;
  if (a > b) return 1;
  return 0;
}

export function sortSpaces(spaces: readonly SpaceView[]): SpaceView[] {
  return [...spaces].sort((a, b) => {
    const nameA = folderName(a.key.path);
    const nameB = folderName(b.key.path);
    return (
      before(nameA.toLowerCase(), nameB.toLowerCase()) ||
      before(nameA, nameB) ||
      before(a.key.path, b.key.path)
    );
  });
}

export function groupAllTerminals(snapshot: HostsSnapshot): HostGroup[] {
  const groups: HostGroup[] = [];
  for (const host of snapshot.hosts) {
    const spaces = sortSpaces(host.spaces).filter((space) => space.terminals.length > 0);
    if (spaces.length > 0) groups.push({ host, spaces });
  }
  return groups;
}
