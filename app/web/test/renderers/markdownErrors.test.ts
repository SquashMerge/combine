import { describe, expect, it, vi } from 'vitest';

vi.mock('markdown-it', () => ({
  default: class {
    render(): string {
      throw new Error('render exploded');
    }
  },
}));

import { renderMarkdown } from '../../src/renderers/markdown/render';

describe('renderMarkdown failures', () => {
  it('resolves to a failure when markdown-it throws', async () => {
    await expect(renderMarkdown('# x')).resolves.toEqual({
      ok: false,
      error: { message: 'render exploded' },
    });
  });
});
