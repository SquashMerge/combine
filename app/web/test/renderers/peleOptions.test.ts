import { beforeEach, describe, expect, it, vi } from 'vitest';

const pele = vi.hoisted(() => ({ render: vi.fn() }));

vi.mock('pele', () => ({
  render: pele.render,
  PeleError: class PeleError extends Error {},
}));

import { renderMermaid } from '../../src/renderers/mermaid/render';

beforeEach(() => {
  pele.render.mockReset();
  pele.render.mockResolvedValue({ svg: '<svg xmlns="http://www.w3.org/2000/svg"></svg>' });
});

describe('Pele render options', () => {
  it('passes exactly the pinned limits, schemes and a prefix, with no math', async () => {
    await renderMermaid('flowchart TD\n A-->B');
    expect(pele.render).toHaveBeenCalledTimes(1);
    const [source, options] = pele.render.mock.calls[0] as [string, Record<string, unknown>];
    expect(source).toBe('flowchart TD\n A-->B');
    expect(options).toEqual({
      limit: 204800,
      maxEdges: 51200,
      outputLimit: 4000000,
      linkSchemes: ['https', 'http'],
      imageSchemes: ['data'],
      idPrefix: expect.any(String),
    });
    expect(Object.keys(options).sort()).toEqual(
      ['idPrefix', 'imageSchemes', 'limit', 'linkSchemes', 'maxEdges', 'outputLimit'].sort(),
    );
    expect('math' in options).toBe(false);
    expect(options.idPrefix).not.toBe('');
  });

  it('gives each call a distinct idPrefix', async () => {
    await renderMermaid('flowchart TD\n A-->B');
    await renderMermaid('flowchart TD\n A-->B');
    const prefixes = pele.render.mock.calls.map(
      (call) => (call[1] as { idPrefix: string }).idPrefix,
    );
    expect(prefixes).toHaveLength(2);
    expect(new Set(prefixes).size).toBe(2);
  });
});
