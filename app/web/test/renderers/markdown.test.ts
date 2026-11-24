import { describe, expect, it } from 'vitest';
import type { RenderOutcome } from '../../src/model/render';
import { renderMarkdown } from '../../src/renderers/markdown/render';
import { OPEN_URL_ATTRIBUTE } from '../../src/renderers/links';
import { MARKDOWN_FIXTURES } from '../fixtures/renderers';

function fixture(id: string): string {
  const found = MARKDOWN_FIXTURES.find((entry) => entry.id === id);
  if (!found) throw new Error(`no fixture ${id}`);
  return found.source;
}

async function rendered(source: string): Promise<DocumentFragment> {
  const outcome = await renderMarkdown(source);
  if (!outcome.ok) throw new Error(`render failed: ${outcome.error.message}`);
  return outcome.fragment;
}

function errorOf(outcome: RenderOutcome) {
  if (outcome.ok) throw new Error('expected a failure');
  return outcome.error;
}

function markers(fragment: DocumentFragment): string[] {
  return [...fragment.querySelectorAll(`[${OPEN_URL_ATTRIBUTE}]`)].map(
    (el) => el.getAttribute(OPEN_URL_ATTRIBUTE) ?? '',
  );
}

describe('renderMarkdown', () => {
  it('wraps the output in div.combine-markdown', async () => {
    const fragment = await rendered('# x');
    expect(fragment.childElementCount).toBe(1);
    const root = fragment.firstElementChild as Element;
    expect(root.localName).toBe('div');
    expect(root.classList.contains('combine-markdown')).toBe(true);
    expect(root.querySelector('h1')?.textContent).toBe('x');
  });

  it('MD1: raw script stays text, as a block and inline', async () => {
    const fragment = await rendered(fixture('MD1'));
    expect(fragment.querySelector('script')).toBeNull();
    expect(fragment.textContent).toContain('<script>alert(1)</script>');
  });

  it('MD2: raw img and div handlers come out as text', async () => {
    const fragment = await rendered(fixture('MD2'));
    expect(fragment.querySelector('img')).toBeNull();
    expect(fragment.querySelector('[onclick]')).toBeNull();
    expect(fragment.textContent).toContain('<img src=x onerror=alert(1)>');
    expect(fragment.textContent).toContain('<div onclick="x">');
  });

  it('MD3: only openable links get a marker, and no href survives', async () => {
    const fragment = await rendered(fixture('MD3'));
    expect(markers(fragment)).toEqual([
      'https://example.com/k',
      'http://example.com/l',
      'https://example.com/auto',
      'https://example.com/m',
    ]);
    expect(fragment.querySelector('[href]')).toBeNull();
  });

  it('MD4: only allowed data images remain', async () => {
    const fragment = await rendered(fixture('MD4'));
    const sources = [...fragment.querySelectorAll('img')].map((img) => img.getAttribute('src'));
    expect(sources).toEqual([
      'data:image/png;base64,iVBORw0KGgo=',
      'data:image/jpeg;base64,/9j/',
      'data:image/gif;base64,R0lGODlh',
      'data:image/webp;base64,UklGRg==',
    ]);
  });

  it('MD6: a hostile fence info string makes no img and no handler', async () => {
    const fragment = await rendered(fixture('MD6'));
    expect(fragment.querySelector('img')).toBeNull();
    expect(fragment.querySelector('[onerror]')).toBeNull();
  });

  it('MD9: an entity-encoded javascript: link has no marker and no href', async () => {
    const fragment = await rendered(fixture('MD9'));
    expect(markers(fragment)).toEqual([]);
    expect(fragment.querySelector('[href]')).toBeNull();
  });

  describe('highlighting', () => {
    const samples: Array<[string, string]> = [
      ['bash', 'echo "$HOME"'],
      ['json', '{"a": 1}'],
      ['rust', 'fn main() {}'],
      ['typescript', 'const x: number = 1;'],
      ['javascript', 'const x = 1;'],
      ['python', 'def f(): pass'],
      ['toml', '[t]\nk = "v"'],
      ['yaml', 'key: value'],
      ['diff', '-a\n+b'],
    ];

    it.each(samples)('highlights %s', async (lang, code) => {
      const fragment = await rendered(`\`\`\`${lang}\n${code}\n\`\`\``);
      const spans = [...fragment.querySelectorAll('pre code span')];
      expect(spans.some((span) => span.className.startsWith('hljs-'))).toBe(true);
    });

    const plain: Array<[string, string, string]> = [
      ['ini', '[t]\nk = "v"', '```ini\n[t]\nk = "v"\n```'],
      ['cobol', 'DISPLAY "x".', '```cobol\nDISPLAY "x".\n```'],
      ['html', '<b>x</b>', '```html\n<b>x</b>\n```'],
      ['no language', 'plain', '```\nplain\n```'],
    ];

    it.each(plain)('leaves %s as escaped text', async (_name, code, markdown) => {
      const fragment = await rendered(markdown);
      const block = fragment.querySelector('pre code') as Element;
      expect(block.querySelector('span')).toBeNull();
      expect(block.textContent).toBe(`${code}\n`);
    });

    it('keeps the alignment style of a table cell', async () => {
      const fragment = await rendered('| a |\n| ---: |\n| 1 |');
      expect(fragment.querySelector('th')?.getAttribute('style')).toBe('text-align:right');
    });
  });

  describe('size', () => {
    it('does not refuse exactly 204,800 bytes', async () => {
      const outcome = await renderMarkdown('a'.repeat(204_800));
      expect(outcome.ok).toBe(true);
    });

    it('refuses 204,801 ASCII bytes', async () => {
      const error = errorOf(await renderMarkdown('a'.repeat(204_801)));
      expect(error).toEqual({ message: 'The source is larger than 204,800 bytes.' });
    });

    it('counts bytes, not UTF-16 units', async () => {
      const error = errorOf(await renderMarkdown(String.fromCharCode(0x20ac).repeat(68_267)));
      expect(error.message).toBe('The source is larger than 204,800 bytes.');
    });
  });
});

describe('markdown-it options', () => {
  it('does not linkify a bare URL', async () => {
    const fragment = await rendered('see https://example.com/bare and www.example.org now');
    expect(fragment.querySelector('a')).toBeNull();
    expect(markers(fragment)).toEqual([]);
    expect(fragment.textContent).toContain('https://example.com/bare');
  });
});
