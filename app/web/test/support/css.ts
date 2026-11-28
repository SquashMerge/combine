import { readdirSync, readFileSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';

export const webRoot = resolve(import.meta.dirname, '../..');
export const srcRoot = join(webRoot, 'src');

export interface CssRule {
  selector: string;
  declarations: Record<string, string>;
}

/** Replaces each comment with spaces of the same newlines, so line numbers survive. */
export function stripComments(css: string): string {
  return css.replace(/\/\*[\s\S]*?\*\//g, (comment) => comment.replace(/[^\n]/g, ' '));
}

/** Parses flat rules (no nested at-rules). */
export function parseRules(css: string): CssRule[] {
  const rules: CssRule[] = [];
  for (const match of stripComments(css).matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const declarations: Record<string, string> = {};
    for (const part of (match[2] ?? '').split(';')) {
      const colon = part.indexOf(':');
      if (colon < 0) continue;
      declarations[part.slice(0, colon).trim()] = part.slice(colon + 1).trim();
    }
    rules.push({ selector: (match[1] ?? '').trim().replace(/\s+/g, ' '), declarations });
  }
  return rules;
}

export function filesUnder(dir: string, extensions: readonly string[]): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return filesUnder(path, extensions);
    return extensions.some((extension) => entry.name.endsWith(extension)) ? [path] : [];
  });
}

export function cssFiles(): string[] {
  return filesUnder(srcRoot, ['.css']);
}

export function read(path: string): string {
  return readFileSync(path, 'utf8');
}

export function relativeToWeb(path: string): string {
  return relative(webRoot, path).split(sep).join('/');
}
