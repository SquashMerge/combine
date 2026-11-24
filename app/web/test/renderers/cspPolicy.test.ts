import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { beforeEach, describe, expect, it } from 'vitest';
import { renderMarkdown } from '../../src/renderers/markdown/render';
import { renderMermaid } from '../../src/renderers/mermaid/render';
import { sanitize } from '../../src/renderers/sanitize/sanitize';
import {
  HTML_FIXTURES,
  MARKDOWN_FIXTURES,
  MERMAID_FIXTURES,
  SVG_FIXTURES,
} from '../fixtures/renderers';
import { stubCanvas } from '../support/canvas';
import { cspViolations } from '../support/cspPolicy';

const webRoot = resolve(import.meta.dirname, '../..');
const classSource = readFileSync(join(webRoot, 'test/fixtures/renderers/class.mmd'), 'utf8');

beforeEach(() => {
  stubCanvas();
});

describe('fixtures under the CSP policy', () => {
  it.each([...MERMAID_FIXTURES, { id: 'class.mmd', source: classSource }])(
    'Mermaid $id has no violations',
    async ({ source }) => {
      const outcome = await renderMermaid(source);
      if (outcome.ok) expect(cspViolations(outcome.fragment)).toEqual([]);
    },
  );

  it.each(MARKDOWN_FIXTURES)('Markdown $id has no violations', async ({ source }) => {
    const outcome = await renderMarkdown(source);
    expect(outcome.ok).toBe(true);
    if (outcome.ok) expect(cspViolations(outcome.fragment)).toEqual([]);
  });

  it.each(SVG_FIXTURES)('SVG $id has no violations', ({ source }) => {
    expect(cspViolations(sanitize(source, 'svg'))).toEqual([]);
  });

  it.each(HTML_FIXTURES)('HTML $id has no violations', ({ source }) => {
    expect(cspViolations(sanitize(source, 'html'))).toEqual([]);
  });
});

describe('cspViolations', () => {
  function dom(html: string): DocumentFragment {
    return document.createRange().createContextualFragment(html);
  }

  const violating: Array<[string, string]> = [
    ['script', '<script>1</script>'],
    ['style element', '<style>p{}</style>'],
    ['foreignObject', '<svg><foreignObject></foreignObject></svg>'],
    ['iframe', '<iframe></iframe>'],
    ['object', '<object></object>'],
    ['embed', '<embed>'],
    ['applet', '<applet></applet>'],
    ['link', '<link rel="stylesheet">'],
    ['meta', '<meta name="x">'],
    ['base', '<base>'],
    ['form', '<form></form>'],
    ['use', '<svg><use></use></svg>'],
    ['feImage', '<svg><feImage></feImage></svg>'],
    ['event handler', '<p onclick="x">t</p>'],
    ['href', '<a href="https://example.com">t</a>'],
    ['img src', '<img src="https://evil.example/x.png">'],
    ['img data svg', '<img src="data:image/svg+xml;base64,PHN2Zz4=">'],
    ['srcset', '<img srcset="x 2x">'],
    ['formaction', '<button formaction="x"></button>'],
    ['poster', '<video poster="x"></video>'],
    ['object data', '<p data="x">t</p>'],
    ['ping', '<a ping="x">t</a>'],
    ['cite', '<blockquote cite="x"></blockquote>'],
    ['xlink:href', '<svg xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="https://evil.example/x.png"/></svg>'],
    ['action', '<p action="x">t</p>'],
    ['background', '<p background="x">t</p>'],
    ['codebase', '<p codebase="x">t</p>'],
    ['lowsrc', '<p lowsrc="x">t</p>'],
    ['dynsrc', '<p dynsrc="x">t</p>'],
    ['longdesc', '<p longdesc="x">t</p>'],
    ['usemap', '<p usemap="#m">t</p>'],
    ['manifest', '<p manifest="x">t</p>'],
    ['icon', '<p icon="x">t</p>'],
    ['data attribute', '<p data-id="1">t</p>'],
    ['marker off an a', '<span data-combine-open-url="https://example.com/">t</span>'],
    ['marker with a bad value', '<a data-combine-open-url="javascript:alert(1)">t</a>'],
    ['marker not normalized', '<a data-combine-open-url="https://example.com">t</a>'],
    ['url() in an attribute', '<svg><rect fill="url(https://evil.example/x)"/></svg>'],
    ['url() in style', '<p style="background:url(x)">t</p>'],
    ['@import in style', '<p style="@import x">t</p>'],
    ['image-set in style', '<p style="x:image-set(a)">t</p>'],
    ['image( in style', '<p style="x:image(a)">t</p>'],
    ['cross-fade in style', '<p style="x:cross-fade(a)">t</p>'],
    ['element( in style', '<p style="x:element(a)">t</p>'],
    ['backslash in style', '<p style="x:\\75rl(a)">t</p>'],
    ['javascript: value', '<p title=" java\tscript:alert(1)">t</p>'],
    ['vbscript: value', '<p title="vbscript:x">t</p>'],
  ];

  it.each(violating)('reports %s', (_name, html) => {
    expect(cspViolations(dom(html)).length).toBeGreaterThan(0);
  });

  it('accepts a fragment reference, an allowed data image and a normalized marker', () => {
    const html =
      '<svg><rect fill="url(#g)"/></svg><img src="data:image/png;base64,iVBORw0KGgo=">' +
      '<a data-combine-open-url="https://example.com/">t</a>';
    expect(cspViolations(dom(html))).toEqual([]);
  });

  it('reports a frame element', () => {
    expect(cspViolations(document.createElement('frame')).length).toBeGreaterThan(0);
  });

  it('reports a frameset element', () => {
    expect(cspViolations(document.createElement('frameset')).length).toBeGreaterThan(0);
  });

  it('accepts a single element as the root', () => {
    expect(cspViolations(document.createElement('p'))).toEqual([]);
    const bad = document.createElement('script');
    expect(cspViolations(bad)).not.toEqual([]);
  });
});

describe('tauri.conf.json', () => {
  it('keeps the app CSP', () => {
    const conf = JSON.parse(
      readFileSync(resolve(webRoot, '../src-tauri/tauri.conf.json'), 'utf8'),
    ) as { app: { security: { csp: string } } };
    expect(conf.app.security.csp).toBe(
      "default-src 'self'; connect-src ipc: http://ipc.localhost; style-src 'self' 'unsafe-inline'; img-src 'self' data:",
    );
  });
});
