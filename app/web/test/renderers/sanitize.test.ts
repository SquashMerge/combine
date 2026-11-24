import { describe, expect, it } from 'vitest';
import { OPEN_URL_ATTRIBUTE } from '../../src/renderers/links';
import {
  isAllowedDataImage,
  isSafeCssValue,
  openableUrl,
  sanitize,
} from '../../src/renderers/sanitize/sanitize';
import { HTML_FIXTURES, SVG_FIXTURES } from '../fixtures/renderers';

const XLINK_NS = 'http://www.w3.org/1999/xlink';

function svgFixture(id: string): DocumentFragment {
  const fixture = SVG_FIXTURES.find((entry) => entry.id === id);
  if (!fixture) throw new Error(`no fixture ${id}`);
  return sanitize(fixture.source, 'svg');
}

function htmlFixture(id: string): DocumentFragment {
  const fixture = HTML_FIXTURES.find((entry) => entry.id === id);
  if (!fixture) throw new Error(`no fixture ${id}`);
  return sanitize(fixture.source, 'html');
}

function elements(fragment: DocumentFragment): Element[] {
  return [...fragment.querySelectorAll('*')];
}

function names(fragment: DocumentFragment): string[] {
  return elements(fragment).map((element) => element.localName.toLowerCase());
}

function attributeValues(fragment: DocumentFragment): string[] {
  return elements(fragment).flatMap((element) =>
    [...element.attributes].map((attribute) => attribute.value),
  );
}

function hasAttribute(fragment: DocumentFragment, name: string): boolean {
  return elements(fragment).some((element) =>
    [...element.attributes].some((attribute) => attribute.name.toLowerCase() === name),
  );
}

function markers(fragment: DocumentFragment): Element[] {
  return [...fragment.querySelectorAll(`[${OPEN_URL_ATTRIBUTE}]`)];
}

function hasLink(fragment: DocumentFragment): boolean {
  return (
    hasAttribute(fragment, 'href') ||
    hasAttribute(fragment, 'xlink:href') ||
    markers(fragment).length > 0
  );
}

describe('sanitize with the svg profile', () => {
  it('S1 drops script', () => {
    const fragment = svgFixture('S1');
    expect(names(fragment)).not.toContain('script');
    expect(names(fragment)).toContain('rect');
  });

  it('S2 drops onload and onclick', () => {
    const fragment = svgFixture('S2');
    expect(hasAttribute(fragment, 'onload')).toBe(false);
    expect(hasAttribute(fragment, 'onclick')).toBe(false);
    expect(names(fragment)).toContain('rect');
  });

  it('S3 drops foreignObject and the img inside it', () => {
    const fragment = svgFixture('S3');
    expect(names(fragment)).not.toContain('foreignobject');
    expect(names(fragment)).not.toContain('img');
    expect(hasAttribute(fragment, 'onerror')).toBe(false);
  });

  it.each(['S4', 'S5'])('%s leaves no href and no marker', (id) => {
    expect(hasLink(svgFixture(id))).toBe(false);
  });

  it('S6 turns an https link into a marker without href', () => {
    const fragment = svgFixture('S6');
    const [anchor, ...rest] = markers(fragment);
    expect(rest).toEqual([]);
    expect(anchor?.getAttribute(OPEN_URL_ATTRIBUTE)).toBe('https://example.com/ok');
    expect(anchor?.getAttribute('role')).toBe('link');
    expect(anchor?.getAttribute('tabindex')).toBe('0');
    expect(hasAttribute(fragment, 'href')).toBe(false);
    expect(hasAttribute(fragment, 'xlink:href')).toBe(false);
  });

  it('S7 normalizes the marker URL', () => {
    const [anchor] = markers(svgFixture('S7'));
    expect(anchor?.getAttribute(OPEN_URL_ATTRIBUTE)).toBe('https://example.com/');
  });

  it('S8 marks none of the non-http hrefs and keeps no href', () => {
    expect(hasLink(svgFixture('S8'))).toBe(false);
  });

  it('S9 keeps only the data PNG image', () => {
    const fragment = svgFixture('S9');
    const images = [...fragment.querySelectorAll('image')];
    expect(images).toHaveLength(1);
    expect(images[0]?.getAttribute('href')).toBe('data:image/png;base64,iVBORw0KGgo=');
  });

  it('S10 drops the style element and every external url()', () => {
    const fragment = svgFixture('S10');
    expect(names(fragment)).not.toContain('style');
    expect(attributeValues(fragment).filter((value) => /url\(/i.test(value))).toEqual([]);
  });

  it('S11 keeps local url() references and var()', () => {
    const rect = svgFixture('S11').querySelector('rect');
    expect(rect?.getAttribute('style')).toBe('fill:url(#grad);stroke:var(--pele-line,#000)');
    expect(rect?.getAttribute('marker-end')).toBe('url(#arrow)');
  });

  it('S12 drops use', () => {
    expect(names(svgFixture('S12'))).not.toContain('use');
  });

  it('S13 drops animate and set', () => {
    const fragment = svgFixture('S13');
    expect(names(fragment)).not.toContain('animate');
    expect(names(fragment)).not.toContain('set');
  });

  it('S14 drops the forged marker', () => {
    expect(markers(svgFixture('S14'))).toEqual([]);
  });

  it('S15 drops the escaped url() style', () => {
    const fragment = svgFixture('S15');
    expect(names(fragment)).toContain('rect');
    expect(hasAttribute(fragment, 'style')).toBe(false);
  });

  it('S16 drops iframe, object and embed', () => {
    const present = names(svgFixture('S16'));
    for (const name of ['iframe', 'object', 'embed']) expect(present).not.toContain(name);
  });

  it('S17 keeps the data PNG image without the handler', () => {
    const fragment = svgFixture('S17');
    expect(fragment.querySelectorAll('image')).toHaveLength(1);
    expect(hasAttribute(fragment, 'onerror')).toBe(false);
  });

  it('S18 drops feImage and every URL attribute', () => {
    const fragment = svgFixture('S18');
    expect(names(fragment)).not.toContain('feimage');
    expect(names(fragment)).toContain('rect');
    expect(hasAttribute(fragment, 'href')).toBe(false);
    for (const element of elements(fragment)) {
      expect(element.getAttributeNS(XLINK_NS, 'href')).toBeNull();
    }
    expect(attributeValues(fragment).filter((value) => value.includes('evil.example'))).toEqual(
      [],
    );
  });
});

describe('sanitize with the html profile', () => {
  it('H1 drops script and onclick', () => {
    const fragment = htmlFixture('H1');
    expect(names(fragment)).not.toContain('script');
    expect(hasAttribute(fragment, 'onclick')).toBe(false);
    expect(fragment.querySelector('p')?.textContent).toBe('text');
  });

  it('H2 keeps only the data PNG and GIF images', () => {
    const fragment = htmlFixture('H2');
    const sources = [...fragment.querySelectorAll('img')].map((img) => img.getAttribute('src'));
    expect(sources).toEqual([
      'data:image/png;base64,iVBORw0KGgo=',
      'data:image/gif;base64,R0lGODlhAQABAAAAACw=',
    ]);
    expect(hasAttribute(fragment, 'srcset')).toBe(false);
  });

  it('H3 marks only the https link', () => {
    const fragment = htmlFixture('H3');
    expect(markers(fragment).map((a) => a.getAttribute(OPEN_URL_ATTRIBUTE))).toEqual([
      'https://example.com/a',
    ]);
    expect(fragment.querySelectorAll('a')).toHaveLength(3);
    expect(hasAttribute(fragment, 'href')).toBe(false);
  });

  it('H4 removes iframe, style, form, svg and math', () => {
    const present = names(htmlFixture('H4'));
    for (const name of ['iframe', 'style', 'form', 'svg', 'script', 'math']) {
      expect(present).not.toContain(name);
    }
    expect(hasAttribute(htmlFixture('H4'), 'action')).toBe(false);
  });

  it('H5 drops the url() style', () => {
    const fragment = htmlFixture('H5');
    expect(fragment.querySelector('p')).not.toBeNull();
    expect(hasAttribute(fragment, 'style')).toBe(false);
  });

  it('H6 drops the forged marker', () => {
    expect(markers(htmlFixture('H6'))).toEqual([]);
  });
});

describe('sanitize availability', () => {
  it('does not touch the default DOMPurify instance', async () => {
    const { default: DOMPurify } = await import('dompurify');
    sanitize('<p>x</p>', 'html');
    const result = DOMPurify.sanitize('<a href="https://example.com/">x</a>');
    expect(result).toContain('href="https://example.com/"');
  });
});

describe('openableUrl', () => {
  it.each([
    ['https://example.com/ok', 'https://example.com/ok'],
    ['http://example.com', 'http://example.com/'],
    ['HTTPS://EXAMPLE.COM', 'https://example.com/'],
    ['https://user@example.com', 'https://user@example.com/'],
  ])('accepts %s', (raw, expected) => {
    expect(openableUrl(raw)).toBe(expected);
  });

  it.each([
    [null],
    [''],
    [' https://example.com/'],
    ['https://example.com/ '],
    ['https://exa\u0000mple.com/'],
    ['https://example.com/\u001f'],
    ['https://example.com/\u0009'],
    ['//evil.example/x'],
    ['/relative'],
    ['#frag'],
    ['./rel'],
    ['mailto:a@b.c'],
    ['data:text/html,x'],
    ['javascript:alert(1)'],
    ['file:///etc/passwd'],
    ['ftp://example.com/'],
  ])('rejects %j', (raw) => {
    expect(openableUrl(raw)).toBeNull();
  });
});

describe('isAllowedDataImage', () => {
  it.each([
    'data:image/png;base64,AAAA',
    'data:image/jpeg;base64,AAAA',
    'data:image/gif;base64,AAAA',
    'data:image/webp,AAAA',
  ])('allows %s', (value) => {
    expect(isAllowedDataImage(value)).toBe(true);
  });

  it.each([
    'data:image/svg+xml;base64,PHN2Zz4=',
    'data:text/html,x',
    'DATA:image/png;base64,AAAA',
    'data:image/PNG;base64,AAAA',
    'data:image/png',
    'https://example.com/a.png',
    ' data:image/png;base64,AAAA',
  ])('rejects %s', (value) => {
    expect(isAllowedDataImage(value)).toBe(false);
  });
});

describe('isSafeCssValue', () => {
  it.each([
    'fill:url(#grad)',
    "fill:url( '#grad')",
    'url(#arrow)',
    'stroke:var(--pele-line,#000)',
    '#fff',
    'M0 0L10 10',
  ])('allows %s', (value) => {
    expect(isSafeCssValue(value)).toBe(true);
  });

  it.each([
    'fill:url(https://evil.example/x)',
    'url(x)',
    'URL(//evil.example)',
    '@import "x"',
    'width:expression (alert(1))',
    'background:image-set("x" 1x)',
    'background:image("x")',
    'background:cross-fade(a, b)',
    'background:element(#x)',
    'background:src("x")',
    'background:\\75rl(x)',
  ])('rejects %s', (value) => {
    expect(isSafeCssValue(value)).toBe(false);
  });
});
