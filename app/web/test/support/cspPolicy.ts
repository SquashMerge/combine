const FORBIDDEN_ELEMENTS = new Set([
  'script',
  'style',
  'foreignobject',
  'iframe',
  'frame',
  'frameset',
  'object',
  'embed',
  'applet',
  'link',
  'meta',
  'base',
  'form',
  'use',
  'feimage',
]);

const URL_ATTRIBUTES = new Set([
  'href',
  'src',
  'srcset',
  'action',
  'formaction',
  'poster',
  'background',
  'data',
  'ping',
  'codebase',
  'lowsrc',
  'dynsrc',
  'longdesc',
  'usemap',
  'manifest',
  'cite',
  'icon',
]);

const MARKER = 'data-combine-open-url';
const ALLOWED_DATA_IMAGE = /^data:image\/(png|jpeg|gif|webp)[;,]/;
const WHITESPACE_AND_C0 = /[\s\u0000-\u001f]/g;

function isOpenableUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return (url.protocol === 'https:' || url.protocol === 'http:') && url.href === value;
  } catch {
    return false;
  }
}

function elementViolations(element: Element): string[] {
  const found: string[] = [];
  const name = element.localName.toLowerCase();
  if (FORBIDDEN_ELEMENTS.has(name)) found.push(`element <${name}>`);
  for (const attribute of element.attributes) {
    const attr = attribute.name.toLowerCase();
    const { value } = attribute;
    const label = `<${name} ${attr}>`;
    if (attr.startsWith('on')) found.push(`event handler ${label}`);
    if (URL_ATTRIBUTES.has(attr) || attr.endsWith(':href')) {
      const allowedImage =
        ALLOWED_DATA_IMAGE.test(value) &&
        ((name === 'img' && attr === 'src') ||
          (name === 'image' && (attr === 'href' || attr === 'xlink:href')));
      if (!allowedImage) found.push(`URL attribute ${label}`);
    }
    if (attr.startsWith('data-') && attr !== MARKER) found.push(`data attribute ${label}`);
    if (attr === MARKER && (name !== 'a' || !isOpenableUrl(value))) {
      found.push(`link marker ${label}`);
    }
    if (/url\((?!#)/i.test(value)) found.push(`url() in ${label}`);
    if (attr === 'style' && /@import|image-set\(|image\(|cross-fade\(|element\(|\\/i.test(value)) {
      found.push(`style resource ${label}`);
    }
    if (/^(javascript|vbscript):/i.test(value.replace(WHITESPACE_AND_C0, ''))) {
      found.push(`script URL ${label}`);
    }
  }
  return found;
}

/** The policy violations in a fragment or element; empty when the markup is inert under the app CSP. */
export function cspViolations(root: DocumentFragment | Element): string[] {
  const elements = [...(root instanceof Element ? [root] : []), ...root.querySelectorAll('*')];
  return elements.flatMap(elementViolations);
}
