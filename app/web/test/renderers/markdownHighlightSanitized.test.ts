import { describe, expect, it, vi } from 'vitest';

vi.mock('highlight.js/lib/core', () => ({
  default: {
    registerLanguage: vi.fn(),
    getLanguage: vi.fn(() => ({})),
    highlight: vi.fn(() => ({
      value: '<img src=x onerror=alert(1)><script>alert(1)</script>',
    })),
  },
}));

describe('highlighter output', () => {
  it('is sanitized before it reaches the fragment', async () => {
    const { renderMarkdown } = await import('../../src/renderers/markdown/render');
    const outcome = await renderMarkdown('```js\nx\n```');
    if (!outcome.ok) throw new Error(outcome.error.message);
    expect(outcome.fragment.querySelector('img')).toBeNull();
    expect(outcome.fragment.querySelector('script')).toBeNull();
    expect(outcome.fragment.querySelector('[onerror]')).toBeNull();
    expect(outcome.fragment.querySelector('pre code')).not.toBeNull();
  });
});
