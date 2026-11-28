import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { type MarkedText, plainText } from '../../src/model/markedText';
import { MarkedTextView } from '../../src/panels/MarkedTextView';

function mount(text: MarkedText): HTMLElement {
  const { container } = render(<div><MarkedTextView text={text} /></div>);
  return container.firstElementChild as HTMLElement;
}

describe('MarkedTextView', () => {
  it('renders a string as one text node and no code', () => {
    const root = mount('Loading…');
    expect(root.childNodes).toHaveLength(1);
    expect(root.firstChild?.nodeType).toBe(Node.TEXT_NODE);
    expect(root.querySelector('code')).toBeNull();
    expect(root.textContent).toBe('Loading…');
  });

  it('renders one code element per code part, in order', () => {
    const text: MarkedText = ['Add ', { code: '/a/b' }, ' then ', { code: 'run' }, '.'];
    const root = mount(text);
    expect(Array.from(root.querySelectorAll('code')).map((code) => code.textContent)).toEqual([
      '/a/b',
      'run',
    ]);
    expect(root.textContent).toBe(plainText(text));
  });
});
