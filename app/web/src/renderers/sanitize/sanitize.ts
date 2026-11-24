import DOMPurify from 'dompurify';
import { OPEN_URL_ATTRIBUTE } from '../links';

export type SanitizeProfile = 'svg' | 'html';

const XLINK_NS = 'http://www.w3.org/1999/xlink';

const purifier = DOMPurify(window);

/** The raw href of each `a`, read before DOMPurify trims it, for the post-pass. */
const openable = new WeakMap<Element, string>();

/** The normalized URL of an absolute http(s) link, or null when it isn't one. */
export function openableUrl(raw: string | null): string | null {
  if (!raw || /^\s|\s$/u.test(raw) || /[\u0000-\u001f]/u.test(raw)) return null;
  try {
    const url = new URL(raw);
    return url.protocol === 'https:' || url.protocol === 'http:' ? url.href : null;
  } catch {
    return null;
  }
}

export function isAllowedDataImage(value: string): boolean {
  return /^data:image\/(png|jpeg|gif|webp)[;,]/.test(value);
}

/** False for anything that can fetch a resource, except a `url(#id)` reference. */
export function isSafeCssValue(value: string): boolean {
  return !/\\|@import|expression\s*\(|image-set\(|image\(|cross-fade\(|element\(|src\(|url\((?!\s*['"]?\s*#)/i.test(
    value,
  );
}

const URL_ATTRIBUTES = new Set(['href', 'xlink:href', 'src', 'srcset']);

purifier.addHook('beforeSanitizeAttributes', (node) => {
  if (node.nodeType !== 1 || node.localName !== 'a') return;
  const raw = node.getAttribute('href') ?? node.getAttributeNS(XLINK_NS, 'href');
  const url = openableUrl(raw);
  if (url !== null) openable.set(node, url);
});

purifier.addHook('uponSanitizeAttribute', (node, data) => {
  const name = data.attrName;
  if (name.startsWith('on')) {
    data.keepAttr = false;
  } else if (URL_ATTRIBUTES.has(name)) {
    data.keepAttr =
      ((node.localName === 'img' && name === 'src') ||
        (node.localName === 'image' && (name === 'href' || name === 'xlink:href'))) &&
      isAllowedDataImage(data.attrValue);
  } else if (!isSafeCssValue(data.attrValue)) {
    data.keepAttr = false;
  }
});

const SVG_CONFIG = {
  USE_PROFILES: { svg: true },
  ADD_ATTR: ['role'],
  FORBID_TAGS: ['foreignObject', 'style', 'script'],
  ALLOW_ARIA_ATTR: true,
};

const HTML_CONFIG = {
  ALLOWED_TAGS: [
    'a',
    'blockquote',
    'br',
    'code',
    'em',
    'h1',
    'h2',
    'h3',
    'h4',
    'h5',
    'h6',
    'hr',
    'img',
    'li',
    'ol',
    'p',
    'pre',
    's',
    'span',
    'strong',
    'table',
    'tbody',
    'td',
    'th',
    'thead',
    'tr',
    'ul',
  ],
  ALLOWED_ATTR: ['alt', 'class', 'src', 'start', 'style', 'title'],
  ALLOW_ARIA_ATTR: false,
  FORBID_TAGS: ['foreignObject', 'style', 'script'],
};

const SHARED_CONFIG = {
  RETURN_DOM_FRAGMENT: true as const,
  ALLOW_DATA_ATTR: false,
  ALLOW_UNKNOWN_PROTOCOLS: false,
};

/**
 * Sanitizes untrusted markup into a fragment. Links are carried by the
 * `data-combine-open-url` marker alone; no `href` survives.
 */
export function sanitize(markup: string, profile: SanitizeProfile): DocumentFragment {
  if (!purifier.isSupported) throw new Error('The sanitizer is not available.');
  const fragment = purifier.sanitize(markup, {
    ...SHARED_CONFIG,
    ...(profile === 'svg' ? SVG_CONFIG : HTML_CONFIG),
  });
  for (const img of fragment.querySelectorAll('img')) {
    if (!img.hasAttribute('src')) img.remove();
  }
  for (const image of fragment.querySelectorAll('image')) {
    if (image.getAttribute('href') === null && image.getAttributeNS(XLINK_NS, 'href') === null) {
      image.remove();
    }
  }
  for (const anchor of fragment.querySelectorAll('a')) {
    const url = openable.get(anchor);
    if (url === undefined) continue;
    anchor.setAttribute(OPEN_URL_ATTRIBUTE, url);
    anchor.setAttribute('role', 'link');
    anchor.setAttribute('tabindex', '0');
  }
  return fragment;
}
