import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { attachRenderedLinks } from '../../src/panels/renderedLinks';
import { OPEN_URL_ATTRIBUTE } from '../../src/renderers/links';
import { renderMarkdown } from '../../src/renderers/markdown/render';
import { renderMermaid } from '../../src/renderers/mermaid/render';
import { MERMAID_FIXTURES } from '../fixtures/renderers';
import { stubCanvas } from '../support/canvas';
import { MemoryOpenUrlPort } from '../support/memoryOpenUrlPort';

const MARKDOWN_URL = 'https://example.com/k';
const MERMAID_URL = 'https://example.com/a';

let container: HTMLElement;
let port: MemoryOpenUrlPort;
let detach: () => void;

async function mount(): Promise<void> {
  const markdown = await renderMarkdown(`[k](${MARKDOWN_URL}) and [rel](/rel)`);
  const source = MERMAID_FIXTURES.find((f) => f.id === 'M-link')?.source;
  if (source === undefined) throw new Error('missing M-link fixture');
  const mermaid = await renderMermaid(source);
  if (!markdown.ok || !mermaid.ok) throw new Error('fixtures must render');
  container.append(markdown.fragment, mermaid.fragment);
}

function markdownLink(): HTMLAnchorElement {
  const link = container.querySelector<HTMLAnchorElement>(
    `a[${OPEN_URL_ATTRIBUTE}="${MARKDOWN_URL}"]`,
  );
  if (link === null) throw new Error('no markdown link');
  return link;
}

function mermaidLink(): Element {
  const link = container.querySelector(`svg [${OPEN_URL_ATTRIBUTE}="${MERMAID_URL}"]`);
  if (link === null) throw new Error('no mermaid link');
  return link;
}

function press(target: Element, init: KeyboardEventInit): KeyboardEvent {
  const event = new KeyboardEvent('keydown', {
    bubbles: true,
    cancelable: true,
    ...init,
  });
  target.dispatchEvent(event);
  return event;
}

function click(target: Element): MouseEvent {
  const event = new MouseEvent('click', { bubbles: true, cancelable: true });
  target.dispatchEvent(event);
  return event;
}

beforeEach(async () => {
  stubCanvas();
  container = document.createElement('div');
  document.body.append(container);
  port = new MemoryOpenUrlPort();
  await mount();
  detach = attachRenderedLinks(container, port);
});

afterEach(() => {
  detach();
  container.remove();
});

describe('attachRenderedLinks', () => {
  it('opens a Markdown link on click and keeps the webview in place', () => {
    const before = location.href;
    const event = click(markdownLink());
    expect(port.urls).toEqual([MARKDOWN_URL]);
    expect(event.defaultPrevented).toBe(true);
    expect(location.href).toBe(before);
  });

  it('opens a Mermaid link on click, also from a child element', () => {
    const link = mermaidLink();
    const event = click(link);
    expect(event.defaultPrevented).toBe(true);
    const child = link.firstElementChild ?? link;
    click(child);
    expect(port.urls).toEqual([MERMAID_URL, MERMAID_URL]);
  });

  it('opens a link on a click on its child', () => {
    const link = markdownLink();
    const child = document.createElement('span');
    link.append(child);
    click(child);
    expect(port.urls).toEqual([MARKDOWN_URL]);
  });

  it('opens a link on Enter', () => {
    const event = press(markdownLink(), { key: 'Enter' });
    expect(port.urls).toEqual([MARKDOWN_URL]);
    expect(event.defaultPrevented).toBe(true);
    press(mermaidLink(), { key: 'Enter' });
    expect(port.urls).toEqual([MARKDOWN_URL, MERMAID_URL]);
  });

  it('ignores a repeated or composing Enter', () => {
    press(markdownLink(), { key: 'Enter', repeat: true });
    press(markdownLink(), { key: 'Enter', isComposing: true });
    expect(port.urls).toEqual([]);
  });

  it('ignores Space, focus, hover and the other pointer events', () => {
    const link = markdownLink();
    const space = press(link, { key: ' ' });
    expect(space.defaultPrevented).toBe(false);
    link.dispatchEvent(new FocusEvent('focus'));
    link.dispatchEvent(new FocusEvent('focusin', { bubbles: true }));
    for (const type of ['mouseover', 'pointerdown', 'mousedown', 'auxclick']) {
      link.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true }));
    }
    expect(port.urls).toEqual([]);
  });

  it('prevents an anchor without a marker and makes no call', () => {
    const plain = container.querySelector<HTMLAnchorElement>('a:not([' + OPEN_URL_ATTRIBUTE + '])');
    const anchor = plain ?? container.appendChild(document.createElement('a'));
    anchor.setAttribute('href', 'https://example.com/plain');
    expect(click(anchor).defaultPrevented).toBe(true);
    expect(press(anchor, { key: 'Enter' }).defaultPrevented).toBe(true);
    expect(port.urls).toEqual([]);
  });

  it('ignores a marker outside the container', () => {
    const outside = document.createElement('a');
    outside.setAttribute(OPEN_URL_ATTRIBUTE, 'https://example.com/out');
    document.body.append(outside);
    const event = click(outside);
    outside.remove();
    expect(event.defaultPrevented).toBe(false);
    expect(port.urls).toEqual([]);
  });

  it('ignores a marker on an ancestor outside the container', () => {
    const host = document.createElement('a');
    host.setAttribute(OPEN_URL_ATTRIBUTE, 'https://example.com/out');
    const inner = document.createElement('div');
    const anchor = document.createElement('a');
    inner.append(anchor);
    host.append(inner);
    document.body.append(host);
    const nested = attachRenderedLinks(inner, port);
    click(anchor);
    nested();
    host.remove();
    expect(port.urls).toEqual([]);
  });

  it('leaves an outer anchor alone when the click is inside the container on non-link content', () => {
    const outer = document.createElement('a');
    outer.setAttribute('href', 'https://example.com/outer');
    const inner = document.createElement('div');
    const text = document.createElement('span');
    inner.append(text);
    outer.append(inner);
    document.body.append(outer);
    const nested = attachRenderedLinks(inner, port);
    const event = click(text);
    const key = press(text, { key: 'Enter' });
    nested();
    outer.remove();
    expect(event.defaultPrevented).toBe(false);
    expect(key.defaultPrevented).toBe(false);
    expect(port.urls).toEqual([]);
  });

  it('stops reacting once detached', () => {
    detach();
    const event = click(markdownLink());
    press(markdownLink(), { key: 'Enter' });
    expect(port.urls).toEqual([]);
    expect(event.defaultPrevented).toBe(false);
  });
});
