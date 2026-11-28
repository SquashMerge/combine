import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { TerminalState } from '../../src/model/hosts';
import {
  StateGlyph,
  panelStatusShape,
  terminalStateShape,
  type GlyphShape,
} from '../../src/panels/StateGlyph';

const SHAPES: readonly GlyphShape[] = [
  'filled-dot',
  'half-ring',
  'ring',
  'square',
  'dashed-ring',
  'cross',
  'check',
];

const TERMINAL_STATES: readonly TerminalState[] = ['running', 'waiting', 'idle', 'exited', 'unknown'];

describe('terminalStateShape', () => {
  it('maps every terminal state, each to its own shape', () => {
    const shapes = TERMINAL_STATES.map((state) => terminalStateShape(state));
    expect(shapes).toEqual(['filled-dot', 'half-ring', 'ring', 'square', 'dashed-ring']);
    expect(new Set(shapes).size).toBe(TERMINAL_STATES.length);
  });

  it('draws a cross for gone and nothing for another word', () => {
    expect(terminalStateShape('gone')).toBe('cross');
    expect(terminalStateShape('working')).toBeNull();
  });
});

describe('panelStatusShape', () => {
  it('maps every status kind', () => {
    expect(panelStatusShape('exited')).toBe('square');
    for (const kind of ['gone', 'lost', 'error', 'streamFailed'] as const) {
      expect(panelStatusShape(kind), kind).toBe('cross');
    }
    expect(panelStatusShape('closed')).toBeNull();
    expect(panelStatusShape(undefined)).toBeNull();
  });
});

describe('StateGlyph', () => {
  it.each(SHAPES)('draws %s as a decorative 12 by 12 svg with no text', (shape) => {
    const { container } = render(<StateGlyph shape={shape} />);
    const svg = container.querySelector('svg');
    expect(svg?.getAttribute('data-glyph')).toBe(shape);
    expect(svg?.getAttribute('class')).toBe('state-glyph');
    expect(svg?.getAttribute('aria-hidden')).toBe('true');
    expect(svg?.getAttribute('focusable')).toBe('false');
    expect(svg?.getAttribute('viewBox')).toBe('0 0 12 12');
    expect(svg?.getAttribute('width')).toBe('12');
    expect(svg?.getAttribute('height')).toBe('12');
    expect(svg?.querySelector('title')).toBeNull();
    expect(container.textContent).toBe('');
    expect(svg?.children.length).toBeGreaterThan(0);
  });

  it('draws nothing for a null shape', () => {
    const { container } = render(<StateGlyph shape={null} />);
    expect(container.firstChild).toBeNull();
  });
});
