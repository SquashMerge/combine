import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { render } from 'pele';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { RenderOutcome } from '../../src/model/render';
import { OPEN_URL_ATTRIBUTE } from '../../src/renderers/links';
import { renderMermaid } from '../../src/renderers/mermaid/render';
import { sanitize } from '../../src/renderers/sanitize/sanitize';
import { MERMAID_FIXTURES } from '../fixtures/renderers';
import { stubCanvas } from '../support/canvas';
import { cspViolations } from '../support/cspPolicy';

const SVG_NS = 'http://www.w3.org/2000/svg';
const fixtureDir = resolve(import.meta.dirname, '../fixtures/renderers');
const cssPath = resolve(import.meta.dirname, '../../src/renderers/mermaid/mermaid.css');

const classSource = readFileSync(join(fixtureDir, 'class.mmd'), 'utf8');
const brokenSource = readFileSync(join(fixtureDir, 'broken-syntax.mmd'), 'utf8');

beforeEach(() => {
  stubCanvas();
});

function fixture(id: string): string {
  const found = MERMAID_FIXTURES.find((entry) => entry.id === id);
  if (!found) throw new Error(`no fixture ${id}`);
  return found.source;
}

function fragmentOf(outcome: RenderOutcome): DocumentFragment {
  if (!outcome.ok) throw new Error(`render failed: ${outcome.error.message}`);
  return outcome.fragment;
}

function errorOf(outcome: RenderOutcome) {
  if (outcome.ok) throw new Error('expected a failure');
  return outcome.error;
}

describe('renderMermaid', () => {
  it('renders the class diagram to sanitized SVG', async () => {
    const fragment = fragmentOf(await renderMermaid(classSource));
    expect(fragment.childElementCount).toBe(1);
    const root = fragment.firstElementChild as Element;
    expect(root.localName).toBe('div');
    expect(root.classList.contains('combine-mermaid')).toBe(true);
    expect(root.children).toHaveLength(1);
    const svgs = root.querySelectorAll('svg');
    expect(svgs).toHaveLength(1);
    expect(svgs[0]?.namespaceURI).toBe(SVG_NS);
    expect(root.textContent).toContain('Animal');
    expect(root.textContent).toContain('Dog');
    expect(
      [...root.querySelectorAll('[style]')].some((el) =>
        el.getAttribute('style')?.includes('var(--pele-'),
      ),
    ).toBe(true);
    expect(cspViolations(fragment)).toEqual([]);
  });

  describe('Pele output shape', () => {
    const DROPPED = new Set(['href', 'xlink:href', 'src', 'srcset', 'rel', 'target']);

    function attributeNames(element: Element): Set<string> {
      return new Set([...element.attributes].map((attr) => attr.name.toLowerCase()));
    }

    it.each([
      ['class.mmd', () => classSource],
      ['M-link', () => fixture('M-link')],
    ])('%s: sanitizing drops only the expected attributes', async (_name, source) => {
      const raw = (
        await render(source(), {
          linkSchemes: ['https', 'http'],
          imageSchemes: ['data'],
          idPrefix: 'shape-',
        })
      ).svg;
      expect(raw).not.toContain('<style');
      expect(raw.toLowerCase()).not.toContain('foreignobject');
      expect(raw).not.toContain('xlink:href');

      const rawDoc = new DOMParser().parseFromString(raw, 'image/svg+xml');
      const rawElements = [...rawDoc.querySelectorAll('*')];
      expect(rawElements.length).toBeGreaterThan(3);
      const fragment = sanitize(raw, 'svg');
      const cleanElements = [...fragment.querySelectorAll('*')];
      expect(cleanElements.map((el) => el.localName)).toEqual(rawElements.map((el) => el.localName));

      rawElements.forEach((rawEl, index) => {
        const cleanEl = cleanElements[index] as Element;
        const expected = new Set(
          [...attributeNames(rawEl)].filter(
            (name) => !DROPPED.has(name) && !name.startsWith('data-'),
          ),
        );
        if (cleanEl.hasAttribute(OPEN_URL_ATTRIBUTE)) {
          expect(cleanEl.localName).toBe('a');
          expected.add(OPEN_URL_ATTRIBUTE);
          expected.add('role');
          expected.add('tabindex');
        }
        expect(attributeNames(cleanEl)).toEqual(expected);
      });

      expect(cleanElements[0]?.localName).toBe('svg');
      expect(cleanElements[0]?.hasAttribute('role')).toBe(true);
      expect(
        cleanElements.some((el) => el.getAttribute('style')?.includes('var(--pele-')),
      ).toBe(true);
    });
  });

  it('sets the theme on .combine-mermaid and leaves the other variables unset', () => {
    const css = readFileSync(cssPath, 'utf8');
    const rule = /\.combine-mermaid\s*\{([^}]*)\}/.exec(css)?.[1] ?? '';
    const mapping: Record<string, string> = {
      '--pele-bg': 'Canvas',
      '--pele-fg': 'CanvasText',
      '--pele-muted': 'GrayText',
      '--pele-line': 'CanvasText',
      '--pele-surface': 'Canvas',
      '--pele-surface-alt': 'ButtonFace',
      '--pele-border': 'CanvasText',
      '--pele-accent': 'Highlight',
      '--pele-error': '#b42318',
      '--pele-critical': '#b42318',
    };
    for (const [name, value] of Object.entries(mapping)) {
      expect(rule).toContain(`${name}: ${value};`);
    }
    expect(rule).toContain('background: Canvas;');
    expect(rule).toContain('color: CanvasText;');
    for (const name of ['--pele-font', '--pele-font-mono', '--pele-radius', '--pele-fg-on-', '--pele-series-', '--pele-zoom-']) {
      expect(css).not.toContain(name);
    }
    expect(css).not.toContain(':root');
    expect(css).not.toContain('color-scheme');
  });

  describe('hostile fixtures', () => {
    for (const { id, source, mustRender, mustFail } of MERMAID_FIXTURES) {
      if (id === 'M11') continue;
      it(`${id} is inert`, async () => {
        const outcome = await renderMermaid(source);
        if (mustRender) expect(outcome.ok).toBe(true);
        if (mustFail) {
          expect(outcome.ok).toBe(false);
          expect(errorOf(outcome).message).not.toBe('');
        }
        if (outcome.ok) expect(cspViolations(outcome.fragment)).toEqual([]);
      });
    }

    it('M-link yields one marker and no href', async () => {
      const fragment = fragmentOf(await renderMermaid(fixture('M-link')));
      const markers = fragment.querySelectorAll(`[${OPEN_URL_ATTRIBUTE}]`);
      expect(markers).toHaveLength(1);
      expect(markers[0]?.getAttribute(OPEN_URL_ATTRIBUTE)).toBe('https://example.com/a');
      expect(fragment.querySelector('[href]')).toBeNull();
    });

    it('M10 has no foreignObject', async () => {
      const outcome = await renderMermaid(fixture('M10'));
      if (outcome.ok) {
        expect(outcome.fragment.querySelector('foreignObject')).toBeNull();
        expect(outcome.fragment.querySelector('foreignobject')).toBeNull();
      }
    });

    it('M11 renders, with no url() other than a fragment reference in any attribute', async () => {
      const fragment = fragmentOf(await renderMermaid(fixture('M11')));
      for (const element of fragment.querySelectorAll('*')) {
        for (const attribute of element.attributes) {
          expect(attribute.value).not.toMatch(/url\((?!\s*['"]?\s*#)/i);
        }
      }
    });
  });

  describe('errors and size', () => {
    it('reports the position of a syntax error', async () => {
      const error = errorOf(await renderMermaid(brokenSource));
      expect(error.message).not.toBe('');
      expect(error.line).toBe(9);
      expect(error.column).toBe(3);
    });

    it('reports an undetectable diagram without a position', async () => {
      const error = errorOf(await renderMermaid('notADiagram'));
      expect(error).toEqual({ message: 'No diagram type detected for the given text.' });
      expect(Object.keys(error)).toEqual(['message']);
    });

    it('reports invalid front matter without a position', async () => {
      const error = errorOf(await renderMermaid('---\ntitle: [x\n---\nclassDiagram\n  class A'));
      expect(error.message).toContain('Front matter is not valid YAML');
      expect(Object.keys(error)).toEqual(['message']);
    });

    it('does not refuse a source of exactly 204,800 bytes for size', async () => {
      const head = 'classDiagram\n  class A\n%% ';
      const outcome = await renderMermaid(head + 'x'.repeat(204_800 - head.length));
      if (!outcome.ok) expect(outcome.error.message).not.toContain('larger than');
    });

    it('refuses 204,801 ASCII bytes', async () => {
      const error = errorOf(await renderMermaid('a'.repeat(204_801)));
      expect(error).toEqual({ message: 'The source is larger than 204,800 bytes.' });
    });

    it('counts bytes, not UTF-16 units', async () => {
      const source = String.fromCharCode(0x20ac).repeat(68_267);
      expect(source.length).toBeLessThan(204_800);
      const error = errorOf(await renderMermaid(source));
      expect(error.message).toBe('The source is larger than 204,800 bytes.');
    });
  });
});
