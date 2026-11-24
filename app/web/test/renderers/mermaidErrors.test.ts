import { beforeEach, describe, expect, it, vi } from 'vitest';

const pele = vi.hoisted(() => ({ render: vi.fn() }));

vi.mock('pele', () => ({
  render: pele.render,
  PeleError: class PeleError extends Error {},
}));

import { renderMermaid } from '../../src/renderers/mermaid/render';

beforeEach(() => {
  pele.render.mockReset();
});

describe('renderMermaid failures', () => {
  it('maps a thrown Error to its message', async () => {
    pele.render.mockImplementation(async () => {
      throw new TypeError('boom');
    });
    await expect(renderMermaid('x')).resolves.toEqual({ ok: false, error: { message: 'boom' } });
  });

  it('maps a thrown non-Error to the generic message', async () => {
    pele.render.mockImplementation(async () => {
      throw 'str';
    });
    await expect(renderMermaid('x')).resolves.toEqual({
      ok: false,
      error: { message: 'The source could not be rendered.' },
    });
  });

  it('maps a synchronous throw to a failure too', async () => {
    pele.render.mockImplementation(() => {
      throw new Error('sync');
    });
    await expect(renderMermaid('x')).resolves.toEqual({ ok: false, error: { message: 'sync' } });
  });

  it('fails when sanitizing leaves no svg', async () => {
    pele.render.mockResolvedValue({ svg: '<script>alert(1)</script>' });
    await expect(renderMermaid('x')).resolves.toEqual({
      ok: false,
      error: { message: 'The diagram has no SVG after sanitizing.' },
    });
  });
});
