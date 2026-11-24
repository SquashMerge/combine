import { readdirSync, readFileSync } from "node:fs";
import { join, posix, relative, resolve, sep } from "node:path";
import { describe, expect, it } from "vitest";

const srcRoot = resolve(import.meta.dirname, "../../src");

const LAZY_DIRS = [
  "renderers/mermaid/",
  "renderers/markdown/",
  "renderers/sanitize/",
];
const RENDERER_LIBRARIES = ["pele", "markdown-it", "highlight.js", "dompurify"];
const LAZY_ENTRY = "renderers/lazy.ts";
const ALLOWED_DYNAMIC = ["./mermaid/render", "./markdown/render"];
const ENTRY = "main.ts";

type FileMap = ReadonlyMap<string, string>;

interface Violation {
  rule: 1 | 2 | 3 | 4;
  file: string;
  detail: string;
}

interface StaticImport {
  specifier: string;
  typeOnly: boolean;
}

interface DynamicImport {
  /** Null when the argument is not a string literal. */
  specifier: string | null;
}

const WORDS = String.raw`(?:(?!\b(?:import|export)\b)[\w\s*,$]|\{[^}]*\})*?`;
const staticPattern = new RegExp(
  String.raw`^[ \t]*((?:import|export)${WORDS})\bfrom\s*(['"])([^'"\n]+)\2|^[ \t]*(import)\s*(['"])([^'"\n]+)\5`,
  "gm",
);
const typeOnlyPattern = /^(?:import|export)\s+type\b/;
const dynamicPattern = /\bimport\s*\(\s*([^)]*?)\s*\)/g;
const literalPattern = /^(['"])([^'"\n]+)\1$/;

function stripComments(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|[^:'"\\])\/\/.*$/gm, "$1");
}

function staticImports(source: string): StaticImport[] {
  return [...stripComments(source).matchAll(staticPattern)].map((m) =>
    m[3] !== undefined
      ? { specifier: m[3], typeOnly: typeOnlyPattern.test(m[1] ?? "") }
      : { specifier: m[6] ?? "", typeOnly: false },
  );
}

function dynamicImports(source: string): DynamicImport[] {
  return [...stripComments(source).matchAll(dynamicPattern)].map((m) => ({
    specifier: literalPattern.exec(m[1] ?? "")?.[2] ?? null,
  }));
}

function packageName(specifier: string): string {
  const parts = specifier.split("/");
  return specifier.startsWith("@")
    ? parts.slice(0, 2).join("/")
    : (parts[0] ?? "");
}

function isLibrary(specifier: string): boolean {
  return (
    !specifier.startsWith(".") &&
    RENDERER_LIBRARIES.includes(packageName(specifier))
  );
}

function inLazyDir(path: string): boolean {
  return LAZY_DIRS.some((dir) => path.startsWith(dir));
}

/** The path a relative specifier points at, as a file in the map when there is one. */
function resolveSpecifier(
  from: string,
  specifier: string,
  files: FileMap,
): string | null {
  if (!specifier.startsWith(".")) return null;
  const base = posix.normalize(posix.join(posix.dirname(from), specifier));
  const found = [
    base,
    `${base}.ts`,
    `${base}.tsx`,
    `${base}/index.ts`,
    `${base}/index.tsx`,
  ].find((candidate) => files.has(candidate));
  return found ?? base;
}

function lazyCheck(files: FileMap): Violation[] {
  const violations: Violation[] = [];
  const add = (rule: Violation["rule"], file: string, detail: string): void => {
    violations.push({ rule, file, detail });
  };

  for (const [file, source] of files) {
    const fromLazy = inLazyDir(file);
    const statics = staticImports(source);
    const dynamics = dynamicImports(source);

    for (const { specifier } of [...statics, ...dynamics]) {
      if (specifier !== null && isLibrary(specifier) && !fromLazy) {
        add(1, file, specifier);
      }
    }
    for (const { specifier, typeOnly } of statics) {
      const target = resolveSpecifier(file, specifier, files);
      if (!typeOnly && target !== null && inLazyDir(target) && !fromLazy) {
        add(2, file, specifier);
      }
    }
    for (const { specifier } of dynamics) {
      if (specifier === null) {
        add(3, file, "import() with a non-literal argument");
        continue;
      }
      const target = resolveSpecifier(file, specifier, files);
      if (target === null || !inLazyDir(target)) continue;
      if (file !== LAZY_ENTRY || !ALLOWED_DYNAMIC.includes(specifier)) {
        add(3, file, specifier);
      }
    }
  }

  const entrySpecifiers = dynamicImports(files.get(LAZY_ENTRY) ?? "").map(
    (d) => d.specifier,
  );
  for (const allowed of ALLOWED_DYNAMIC) {
    if (entrySpecifiers.filter((s) => s === allowed).length !== 1) {
      add(3, LAZY_ENTRY, `expected exactly one import('${allowed}')`);
    }
  }

  const seen = new Set<string>();
  const queue = [ENTRY];
  for (let file = queue.pop(); file !== undefined; file = queue.pop()) {
    if (seen.has(file)) continue;
    seen.add(file);
    for (const { specifier, typeOnly } of staticImports(
      files.get(file) ?? "",
    )) {
      if (typeOnly) continue;
      if (isLibrary(specifier)) {
        add(4, file, specifier);
        continue;
      }
      const target = resolveSpecifier(file, specifier, files);
      if (target === null) continue;
      if (inLazyDir(target)) add(4, file, target);
      else if (files.has(target)) queue.push(target);
    }
  }

  return violations;
}

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return /\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

function realFiles(): Map<string, string> {
  return new Map(
    sourceFiles(srcRoot).map((path) => [
      relative(srcRoot, path).split(sep).join("/"),
      readFileSync(path, "utf8"),
    ]),
  );
}

const CLEAN_MAP: [string, string][] = [
  [
    "main.ts",
    "import { App } from './shell/App';\nimport { renderSource } from './renderers/lazy';\n",
  ],
  [
    "shell/App.ts",
    "import type { Renderer } from '../renderers/markdown/render';\nexport const x = 1;\n",
  ],
  [
    "renderers/lazy.ts",
    "export async function f() {\n  await import('./mermaid/render');\n  await import('./markdown/render');\n}\n",
  ],
  [
    "renderers/mermaid/render.ts",
    "import { render } from 'pele';\nimport { s } from '../sanitize/sanitize';\n",
  ],
  [
    "renderers/markdown/render.ts",
    "import MarkdownIt from 'markdown-it';\nimport hljs from 'highlight.js/lib/core';\n",
  ],
  ["renderers/sanitize/sanitize.ts", "import DOMPurify from 'dompurify';\n"],
];

function mapWith(extra: [string, string][]): FileMap {
  return new Map([...CLEAN_MAP, ...extra]);
}

describe("lazy renderer chunking", () => {
  it("keeps the renderers out of the static closure of src/main.ts", () => {
    expect(lazyCheck(realFiles())).toEqual([]);
  });

  describe("self-test on a synthetic file map", () => {
    it("reports nothing for the clean map", () => {
      expect(lazyCheck(mapWith([]))).toEqual([]);
    });

    it("reports a renderer library outside a lazy directory (rule 1)", () => {
      const found = lazyCheck(
        mapWith([["shell/Bad.ts", "import { render } from 'pele';\n"]]),
      );
      expect(found).toContainEqual({
        rule: 1,
        file: "shell/Bad.ts",
        detail: "pele",
      });
    });

    it("reports a library subpath outside a lazy directory (rule 1)", () => {
      const found = lazyCheck(
        mapWith([
          ["shell/Bad.ts", "import hljs from 'highlight.js/lib/core';\n"],
        ]),
      );
      expect(found).toContainEqual({
        rule: 1,
        file: "shell/Bad.ts",
        detail: "highlight.js/lib/core",
      });
    });

    it("reports a static import into a lazy directory (rule 2)", () => {
      const found = lazyCheck(
        mapWith([
          [
            "panels/Bad.ts",
            "import { renderMarkdown } from '../renderers/markdown/render';\n",
          ],
        ]),
      );
      expect(found).toContainEqual({
        rule: 2,
        file: "panels/Bad.ts",
        detail: "../renderers/markdown/render",
      });
    });

    it("reports an `export from` and a side-effect import into a lazy directory (rule 2)", () => {
      const found = lazyCheck(
        mapWith([
          [
            "panels/ReExport.ts",
            "export { renderMermaid } from '../renderers/mermaid/render';\n",
          ],
          ["panels/Effect.ts", "import '../renderers/sanitize/sanitize';\n"],
        ]),
      );
      expect(found).toContainEqual({
        rule: 2,
        file: "panels/ReExport.ts",
        detail: "../renderers/mermaid/render",
      });
      expect(found).toContainEqual({
        rule: 2,
        file: "panels/Effect.ts",
        detail: "../renderers/sanitize/sanitize",
      });
    });

    it("reports `import { type X }` into a lazy directory (rule 2)", () => {
      const found = lazyCheck(
        mapWith([
          [
            "panels/Bad.ts",
            "import { type Renderer } from '../renderers/markdown/render';\n",
          ],
        ]),
      );
      expect(found.map((v) => v.rule)).toContain(2);
    });

    it("exempts `import type` and `export type` statements (rule 2)", () => {
      const found = lazyCheck(
        mapWith([
          [
            "panels/Types.ts",
            "import type { Renderer } from '../renderers/markdown/render';\nexport type { Renderer } from '../renderers/markdown/render';\n",
          ],
        ]),
      );
      expect(found).toEqual([]);
    });

    it("reports an import() into a lazy directory outside lazy.ts (rule 3)", () => {
      const found = lazyCheck(
        mapWith([
          [
            "panels/Bad.ts",
            "const m = import('../renderers/markdown/render');\n",
          ],
        ]),
      );
      expect(found).toContainEqual({
        rule: 3,
        file: "panels/Bad.ts",
        detail: "../renderers/markdown/render",
      });
    });

    it("reports an extra import() in lazy.ts and a non-literal import() (rule 3)", () => {
      const found = lazyCheck(
        new Map([
          ...CLEAN_MAP.filter(([path]) => path !== LAZY_ENTRY),
          [
            LAZY_ENTRY,
            "const a = import('./mermaid/render');\nconst b = import('./markdown/render');\nconst c = import('./sanitize/sanitize');\nconst d = import(name);\n",
          ],
        ]),
      );
      expect(found).toContainEqual({
        rule: 3,
        file: LAZY_ENTRY,
        detail: "./sanitize/sanitize",
      });
      expect(found).toContainEqual({
        rule: 3,
        file: LAZY_ENTRY,
        detail: "import() with a non-literal argument",
      });
    });

    it("reports a missing import() in lazy.ts (rule 3)", () => {
      const found = lazyCheck(
        new Map([
          ...CLEAN_MAP.filter(([path]) => path !== LAZY_ENTRY),
          [LAZY_ENTRY, "const a = import('./mermaid/render');\n"],
        ]),
      );
      expect(found).toContainEqual({
        rule: 3,
        file: LAZY_ENTRY,
        detail: "expected exactly one import('./markdown/render')",
      });
    });

    it("reports a lazy file in the static closure of main.ts (rule 4)", () => {
      const found = lazyCheck(
        mapWith([
          [
            "main.ts",
            "import { x } from './shell/App';\nimport './shell/Chain';\n",
          ],
          [
            "shell/Chain.ts",
            "import { y } from '../renderers/markdown/render';\n",
          ],
        ]),
      );
      expect(found).toContainEqual({
        rule: 4,
        file: "shell/Chain.ts",
        detail: "renderers/markdown/render.ts",
      });
    });

    it("reports a renderer library in the static closure of main.ts (rule 4)", () => {
      const found = lazyCheck(
        mapWith([
          ["main.ts", "import './shell/Chain';\n"],
          ["shell/Chain.ts", "import MarkdownIt from 'markdown-it';\n"],
        ]),
      );
      expect(found).toContainEqual({
        rule: 4,
        file: "shell/Chain.ts",
        detail: "markdown-it",
      });
    });

    it("does not follow import() or type-only imports into the closure (rule 4)", () => {
      const found = lazyCheck(
        mapWith([
          [
            "main.ts",
            "import type { Renderer } from './renderers/markdown/render';\nimport './renderers/lazy';\n",
          ],
        ]),
      );
      expect(found.filter((v) => v.rule === 4)).toEqual([]);
    });
  });
});
