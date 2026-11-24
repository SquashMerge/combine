import { vi } from 'vitest';

/** Keeps jsdom's "not implemented" canvas error out of Pele tests. */
export function stubCanvas(): void {
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
}
