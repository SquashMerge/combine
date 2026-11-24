import type { OpenUrlPort } from '../ipc/port';
import { OPEN_URL_ATTRIBUTE } from '../renderers/links';

/** The anchor under an event target, when it lies inside the container. */
function anchorWithin(container: HTMLElement, target: EventTarget | null): Element | null {
  if (!(target instanceof Element)) return null;
  const anchor = target.closest('a');
  return anchor !== null && container.contains(anchor) ? anchor : null;
}

/**
 * Routes clicks and Enter on rendered links to the system browser. An anchor is never followed
 * in the webview; only one carrying the sanitizer's marker is passed on to the port.
 */
export function attachRenderedLinks(container: HTMLElement, port: OpenUrlPort): () => void {
  const follow = (event: Event): void => {
    if (anchorWithin(container, event.target) === null) return;
    event.preventDefault();
    const target = event.target as Element;
    const marked = target.closest(`[${OPEN_URL_ATTRIBUTE}]`);
    if (marked === null || !container.contains(marked)) return;
    const url = marked.getAttribute(OPEN_URL_ATTRIBUTE);
    if (url !== null) void port.openUrl(url);
  };
  const onKeyDown = (event: KeyboardEvent): void => {
    if (event.key !== 'Enter' || event.repeat || event.isComposing) return;
    follow(event);
  };
  container.addEventListener('click', follow);
  container.addEventListener('keydown', onKeyDown);
  return () => {
    container.removeEventListener('click', follow);
    container.removeEventListener('keydown', onKeyDown);
  };
}
