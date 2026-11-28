import { describe, expect, it } from 'vitest';
import {
  cssFiles,
  filesUnder,
  parseRules,
  read,
  relativeToWeb,
  srcRoot,
  stripComments,
  type CssRule,
} from './support/css';

const TOKEN_CSS = 'src/shell/tokens.css';
const sources = cssFiles().map((path) => ({ file: relativeToWeb(path), css: read(path) }));
const rules: (CssRule & { file: string })[] = sources.flatMap(({ file, css }) =>
  parseRules(css).map((rule) => ({ ...rule, file })),
);

const selectorList = (rule: CssRule): string[] => rule.selector.split(',').map((part) => part.trim());

function rule(selector: string): CssRule {
  const found = rules.find((candidate) => selectorList(candidate).includes(selector));
  if (found === undefined) throw new Error(`no CSS rule for ${selector}`);
  return found;
}

function declarations(selector: string): Record<string, string> {
  return Object.assign(
    {},
    ...rules.filter((candidate) => selectorList(candidate).includes(selector)).map((r) => r.declarations),
  ) as Record<string, string>;
}

describe('base', () => {
  it('sets the page on ink, in the UI face, without ligatures', () => {
    expect(declarations('body')).toMatchObject({
      background: 'var(--ink)',
      color: 'var(--text)',
      font: '400 14px/20px var(--font-ui)',
      'font-variant-ligatures': 'none',
    });
    expect(declarations('html')['color-scheme']).toBe('dark');
    expect(declarations('::selection').background).toBe('var(--selection)');
  });

  it('sets headings in the display face at 700', () => {
    const heading = rules.find((r) => selectorList(r).includes('h1') && selectorList(r).includes('h6'));
    expect(heading?.declarations).toMatchObject({ 'font-family': 'var(--font-display)', 'font-weight': '700' });
  });

  it('sets code in the mono face and lets inline code inherit', () => {
    const code = rules.find((r) => selectorList(r).includes('code') && selectorList(r).includes('pre'));
    expect(code?.declarations['font-family']).toBe('var(--font-mono)');
    expect(declarations('code')).toMatchObject({ 'font-size': 'inherit', 'font-weight': 'inherit', color: 'inherit' });
    expect(declarations('code').background).toBeUndefined();
    expect(declarations('code').padding).toBeUndefined();
  });

  it('inherits the font into form controls', () => {
    const controls = rules.find((r) => selectorList(r).includes('textarea'));
    expect(controls?.declarations).toMatchObject({ font: 'inherit', color: 'inherit' });
  });

  it('no longer bolds every current item', () => {
    expect(rules.some((r) => r.selector === '[aria-current="true"]')).toBe(false);
  });

  it('dims by colour, not opacity', () => {
    expect(declarations('[data-dimmed="true"]')).toEqual({ color: 'var(--text-muted)' });
  });
});

describe('fonts', () => {
  const fontDeclarations = rules
    .filter((r) => r.file !== TOKEN_CSS)
    .flatMap((r) =>
      Object.entries(r.declarations)
        .filter(([property]) => property === 'font-family' || property === 'font')
        .map(([property, value]) => ({ r, property, value })),
    );

  it('takes every font from a token or inherits it', () => {
    expect(fontDeclarations.length).toBeGreaterThan(0);
    for (const { r, value } of fontDeclarations) {
      expect(value, `${r.file}: ${r.selector}`).toMatch(/var\(--font-(?:display|ui|mono)\)|^inherit$/);
    }
  });

  it('uses only the bundled weights', () => {
    for (const r of rules) {
      const weight = r.declarations['font-weight'];
      if (weight !== undefined) expect(['400', '600', '700', 'inherit'], `${r.file}: ${r.selector}`).toContain(weight);
      const shorthand = r.declarations['font'];
      if (shorthand !== undefined && shorthand !== 'inherit') {
        expect(shorthand, `${r.file}: ${r.selector}`).toMatch(/^(?:400|600|700) /);
      }
    }
  });

  it('sets mono and display text at the sites that need them', () => {
    for (const selector of [
      '.terminal-entry .command',
      '.add-terminal .command',
      '.terminal-panel__backend',
      '.terminal-panel__switcher',
      '.view-panel-snippet',
      '.combine-markdown code',
    ]) {
      expect(declarations(selector)['font-family'], selector).toBe('var(--font-mono)');
    }
    for (const selector of ['.rail-brand__name', '.terminal-panel__name']) {
      expect(declarations(selector)['font-family'], selector).toBe('var(--font-display)');
      expect(declarations(selector)['font-weight'], selector).toBe('700');
    }
  });
});

describe('source', () => {
  it('has no nested at-rules and no global box-sizing', () => {
    for (const { file, css } of sources) {
      expect(stripComments(css), file).not.toMatch(/@(?:media|supports|layer|container)/);
    }
    expect(rules.some((r) => r.declarations['box-sizing'] !== undefined && /^(?:\*|html|body)$/.test(r.selector))).toBe(
      false,
    );
  });
});

describe('buttons', () => {
  it('rounds every button to 8px on a control border', () => {
    expect(declarations('button')).toMatchObject({
      'border-radius': '8px',
      border: '1px solid var(--control-border)',
      'min-height': '28px',
    });
  });

  it('dims a disabled button with the shared opacity', () => {
    expect(declarations('button:disabled').opacity).toBe('var(--disabled-opacity)');
  });

  it('puts the primary style on Add terminal alone', () => {
    const hits = filesUnder(srcRoot, ['.ts', '.tsx']).filter((path) => read(path).includes('button-primary'));
    expect(hits.map(relativeToWeb)).toEqual(['src/shell/AddTerminalMenu.tsx']);
    const occurrences = read(hits[0] ?? '').split('button-primary').length - 1;
    expect(occurrences).toBe(1);
  });

  it('keeps amber backgrounds to the primary button', () => {
    const amber = rules.filter((r) =>
      ['background', 'background-color'].some((property) => r.declarations[property]?.includes('var(--amber)')),
    );
    expect(amber.length).toBeGreaterThan(0);
    for (const r of amber) expect(r.selector, r.file).toContain('.button-primary');
    expect(declarations('.button-primary').background).toBe('var(--amber)');
    expect(declarations('.button-primary')['color']).toBe('var(--ink)');
  });

  it('marks the destructive confirm button in the danger colour', () => {
    expect(declarations('.button-danger')).toEqual({ 'border-color': 'var(--danger)', color: 'var(--danger)' });
  });
});

describe('rail', () => {
  it('sits on the surface with the selected space in purple and 600', () => {
    expect(declarations('.shell > nav').background).toBe('var(--surface)');
    expect(declarations('.shell > section').background).toBe('var(--surface)');
    expect(declarations('nav[aria-label="Spaces"] button[aria-current="true"]')).toEqual({
      background: 'var(--purple)',
      color: 'var(--ink)',
      'font-weight': '600',
    });
  });

  it('raises the pressed All spaces toggle with a lavender border and 600', () => {
    expect(declarations('section[aria-label="Terminals"] > button[aria-pressed="true"]')).toEqual({
      background: 'var(--raised)',
      'border-color': 'var(--lavender)',
      'font-weight': '600',
    });
  });

  it('borders the active terminal entry in purple', () => {
    expect(declarations('li[aria-current="true"] > .terminal-entry')).toEqual({
      background: 'var(--raised)',
      'border-color': 'var(--purple)',
    });
  });
});

describe('tabs and tiles', () => {
  it('underlines the shown tab in lavender and bolds it', () => {
    expect(declarations('.tile-bar [role="tab"]')['border-bottom']).toBe('2px solid transparent');
    expect(declarations('.tile-bar [role="tab"][aria-selected="true"]')).toEqual({
      'border-bottom-color': 'var(--lavender)',
      color: 'var(--text)',
      'font-weight': '600',
    });
  });

  it('rounds the tile and draws the focused tile with an inset second border', () => {
    expect(declarations('.tile')).toMatchObject({
      'border-radius': '12px',
      padding: '1px',
      background: 'var(--surface)',
      border: '1px solid var(--raised)',
    });
    expect(declarations('.tile[aria-current="true"]')).toEqual({
      'border-color': 'var(--purple)',
      'box-shadow': 'inset 0 0 0 1px var(--purple)',
    });
  });

  it('raises the selected palette option with a lavender left rule', () => {
    expect(declarations('.palette li')['border-left']).toBe('2px solid transparent');
    expect(declarations('.palette li[aria-selected="true"]')).toMatchObject({
      background: 'var(--raised)',
      'border-left-color': 'var(--lavender)',
    });
  });
});

describe('palette and panel header', () => {
  it('dims the palette backdrop with the scrim', () => {
    expect(declarations('.palette-backdrop').background).toBe('var(--scrim)');
    expect(declarations('.confirm-backdrop').background).toBe('var(--scrim)');
    expect(declarations('.palette')).toMatchObject({ background: 'var(--surface)', 'border-radius': '12px' });
  });

  it('puts the panel header on the surface and its failures in the danger colour', () => {
    expect(declarations('.terminal-panel__header')).toMatchObject({
      background: 'var(--surface)',
      color: 'var(--text-muted)',
      'min-height': '40px',
    });
    for (const status of ['gone', 'lost', 'error', 'streamFailed']) {
      expect(declarations(`.terminal-panel__status [data-status="${status}"]`).color).toBe('var(--danger)');
    }
  });

  it('shows the overlay chip raised', () => {
    expect(declarations('.terminal-view__overlay')).toMatchObject({
      background: 'var(--raised)',
      color: 'var(--text)',
      'border-radius': '8px',
    });
  });

  it('raises the client state error with a danger border', () => {
    expect(declarations('.client-state-error')).toMatchObject({
      background: 'var(--raised)',
      border: '1px solid var(--danger)',
    });
  });
});

describe('focus and opacity', () => {
  it('rings every focused element in lavender, inset inside tiles and the palette', () => {
    expect(declarations(':focus-visible')).toEqual({ outline: '2px solid var(--lavender)', 'outline-offset': '2px' });
    expect(declarations('.tile :focus-visible')['outline-offset']).toBe('-2px');
    expect(declarations('.palette :focus-visible')['outline-offset']).toBe('-2px');
  });

  it('never removes an outline', () => {
    for (const r of rules) {
      for (const property of ['outline', 'outline-style', 'outline-width']) {
        expect(r.declarations[property] ?? 'set', `${r.file}: ${r.selector}`).not.toMatch(/^(?:none|0|0px)$/);
      }
    }
  });

  it('uses opacity only for disabled controls, the held tile and the stale screen', () => {
    const used = rules
      .filter((r) => r.declarations['opacity'] !== undefined)
      .map((r) => `${r.selector} = ${r.declarations['opacity']}`)
      .sort();
    expect(used).toEqual(
      [
        '.layout[data-held] .tile = 0.85',
        '.terminal-view__viewport[data-dimmed="true"] = 0.4',
        'button:disabled = var(--disabled-opacity)',
      ].sort(),
    );
  });
});
