import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const webRoot = resolve(import.meta.dirname, '../..');

const manifest = JSON.parse(readFileSync(join(webRoot, 'package.json'), 'utf8')) as {
  dependencies: Record<string, string>;
};
const lockfile = JSON.parse(readFileSync(join(webRoot, 'package-lock.json'), 'utf8')) as {
  packages: Record<string, { version: string; integrity?: string }>;
};

const RENDERER_PACKAGES = ['pele', 'dompurify', 'markdown-it', 'highlight.js'];
const PELE_INTEGRITY =
  'sha512-Ie8UKY9x00MsuR3jO6AzUwzK2Z6MIV3Tf4EDdITiuk8Uaz8K7g0R082WqsqNaSU8DdgkrRvZThzhit83mi/uiw==';

describe('renderer package pins', () => {
  it.each(RENDERER_PACKAGES)('%s is an exact version in dependencies', (name) => {
    expect(manifest.dependencies[name]).toMatch(/^\d+\.\d+\.\d+$/);
  });

  it('pins markdown-it to a 15.x release', () => {
    expect(manifest.dependencies['markdown-it']).toMatch(/^15\./);
  });

  it.each(RENDERER_PACKAGES)('%s lockfile version equals package.json', (name) => {
    expect(lockfile.packages[`node_modules/${name}`]?.version).toBe(manifest.dependencies[name]);
  });

  it('pins pele 1.13.0 with its recorded integrity', () => {
    expect(manifest.dependencies['pele']).toBe('1.13.0');
    expect(lockfile.packages['node_modules/pele']?.integrity).toBe(PELE_INTEGRITY);
  });
});
