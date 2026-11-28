import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { filesUnder, read, srcRoot, stripComments, webRoot } from './support/css';

const PACKAGES = [
  ['manrope', 'Manrope'],
  ['inter', 'Inter'],
  ['jetbrains-mono', 'JetBrains-Mono'],
] as const;

const FONT_FILES = [
  '@fontsource/manrope/700.css',
  '@fontsource/inter/400.css',
  '@fontsource/inter/600.css',
  '@fontsource/jetbrains-mono/400.css',
  '@fontsource/jetbrains-mono/700.css',
];

const manifest = JSON.parse(read(join(webRoot, 'package.json'))) as {
  dependencies: Record<string, string>;
};

describe('bundled fonts', () => {
  it('pins the three font packages at 5.3.0', () => {
    for (const [pkg] of PACKAGES) expect(manifest.dependencies[`@fontsource/${pkg}`]).toBe('5.3.0');
  });

  it('imports exactly the five per-weight stylesheets', () => {
    const imports = [...read(join(srcRoot, 'shell/fonts.ts')).matchAll(/import\s+'([^']+)';/g)].map(
      (match) => match[1],
    );
    expect(imports).toEqual(FONT_FILES);
    expect(read(join(srcRoot, 'shell/fonts.ts')).match(/\bimport\b/g)).toHaveLength(5);
  });

  it('serves every font file from the package itself', () => {
    for (const file of FONT_FILES) {
      const css = readFileSync(join(webRoot, 'node_modules', file), 'utf8');
      const urls = [...css.matchAll(/url\(([^)]*)\)/g)].map((match) => (match[1] ?? '').replace(/['"]/g, ''));
      expect(urls.length, file).toBeGreaterThan(0);
      for (const url of urls) expect(url, file).toMatch(/^\.\/files\//);
    }
  });

  it('ships each OFL text unchanged', () => {
    for (const [pkg, name] of PACKAGES) {
      const shipped = readFileSync(join(webRoot, 'public/licenses', `OFL-${name}.txt`));
      const original = readFileSync(join(webRoot, 'node_modules/@fontsource', pkg, 'LICENSE'));
      expect(shipped.equals(original), name).toBe(true);
      expect(shipped.toString('utf8')).toContain('SIL OPEN FONT LICENSE');
    }
  });

  it('names no remote font host', () => {
    const files = [...filesUnder(srcRoot, ['.css', '.ts', '.tsx', '.html']), join(webRoot, 'index.html')];
    for (const path of files) {
      const source = read(path);
      expect(source, path).not.toMatch(/fonts\.googleapis|fonts\.gstatic/);
      expect(stripComments(source), path).not.toMatch(/url\(\s*['"]?https?:\/\/|@import[^;]*https?:\/\//);
    }
  });

  it('keeps fonts out of data: URLs', () => {
    expect(read(join(webRoot, 'vite.config.ts'))).toContain('assetsInlineLimit: 0');
  });

  it('loads the fonts before the first render', () => {
    const main = read(join(srcRoot, 'main.ts'));
    expect(main).toContain("import './shell/fonts';");
    expect(main).toContain("import './shell/tokens.css';");
    expect(main.indexOf('renderAfterFonts(')).toBeGreaterThan(-1);
    expect(main.indexOf('renderAfterFonts(')).toBeLessThan(main.indexOf('createRoot('));
  });
});

describe('brand logo', () => {
  it('ships the rail logo as a byte-identical copy of the brand asset', () => {
    const shipped = readFileSync(join(srcRoot, 'shell/combine-icon-small.svg'));
    const original = readFileSync(join(webRoot, '../../docs/brand/combine-icon-small.svg'));
    expect(shipped.equals(original)).toBe(true);
  });
});
