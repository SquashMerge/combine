import { readdirSync, readFileSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';
import { describe, expect, it } from 'vitest';

const webRoot = resolve(import.meta.dirname, '..');
const srcRoot = join(webRoot, 'src');

const manifest = JSON.parse(readFileSync(join(webRoot, 'package.json'), 'utf8')) as {
  dependencies: Record<string, string>;
};

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return /\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

const importPattern =
  /(?:\bfrom\s*|\bimport\s*\(?\s*|\bimport\s+|\brequire\s*\(\s*)(['"])([^'"\n]+)\1/g;

function specifiers(source: string): string[] {
  return [...source.matchAll(importPattern)].map((match) => match[2] ?? '');
}

function packageName(specifier: string): string {
  const parts = specifier.split('/');
  return specifier.startsWith('@') ? parts.slice(0, 2).join('/') : (parts[0] ?? '');
}

const files = sourceFiles(srcRoot).map((path) => ({
  rel: relative(srcRoot, path).split(sep).join('/'),
  specifiers: specifiers(readFileSync(path, 'utf8')),
}));

describe('web source imports', () => {
  it('imports only packages declared in dependencies', () => {
    const bare = files.flatMap((file) =>
      file.specifiers
        .filter((s) => !s.startsWith('.') && !s.startsWith('/') && !s.startsWith('node:'))
        .map((s) => ({ file: file.rel, name: packageName(s) })),
    );
    const undeclared = bare.filter((entry) => !(entry.name in manifest.dependencies));
    expect(undeclared).toEqual([]);
  });

  it('imports @tauri-apps packages only under src/ipc/', () => {
    const outside = files
      .filter((file) => !file.rel.startsWith('ipc/'))
      .filter((file) => file.specifiers.some((s) => s.startsWith('@tauri-apps/')))
      .map((file) => file.rel);
    expect(outside).toEqual([]);
  });

  it('has no node: imports in src', () => {
    const offenders = files
      .filter((file) => file.specifiers.some((s) => s.startsWith('node:')))
      .map((file) => file.rel);
    expect(offenders).toEqual([]);
  });
});
