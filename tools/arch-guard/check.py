#!/usr/bin/env python3
"""Architecture guard: checks the rules .trellis/rules.toml can't express.

    python3 -I tools/arch-guard/check.py            # human output, exit 1 on violations
    python3 -I tools/arch-guard/check.py --json     # machine output

Rules live in tools/arch-guard/guard.toml; SPEC.md ("Repository layout and architecture
rules") says which rows of the architecture table this covers. It reads files only: it
never runs Cargo, npm or the code it checks. Standard library only.

It is a lexical checker, not a compiler. Known limits, by design:
- Rust: comments, string literals and `#[cfg(test)]` / test-support items are removed
  before matching; macros are not expanded; re-exports, glob imports of a parent module
  (`use std::*;`) and aliases of a crate root (`use std as s;`) are not followed.
- Frontend: static `import`/`export ... from`, `import()` and `require()` with a literal
  specifier; computed specifiers are not seen.
- Cargo: workspace members come from the root manifest's `members`/`exclude` globs;
  `[patch]`, `[replace]` and dependencies added by build scripts are not seen.
"""

from __future__ import annotations

import argparse
import bisect
import json
import os
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass, asdict
from pathlib import Path, PurePosixPath

SKIP_DIRS = {".git", "target", "node_modules", "dist", ".reviews"}
RUST_EXT = {".rs"}
JS_EXT = {".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"}
RULE_KINDS = {
    "crate_ban": {"id", "crates", "reason"},
    "crate_scope": {"id", "crates", "packages", "reason"},
    "rust_path": {"id", "in", "except", "forbid", "reason"},
    "rust_ident": {"id", "in", "except", "forbid", "reason"},
    "js_import": {"id", "in", "except", "packages", "reason"},
    "source_language": {"id", "in", "except", "forbid_extensions", "reason"},
    "path_ban": {"id", "to", "reason"},
}
TOP_KEYS = {"version", "non_production"} | set(RULE_KINDS)


@dataclass(frozen=True)
class Violation:
    rule: str
    path: str
    line: int
    message: str


class ConfigError(Exception):
    pass


# --------------------------------------------------------------------------- globs

def glob_regex(glob: str) -> re.Pattern[str]:
    out, i = [], 0
    while i < len(glob):
        if glob.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif glob.startswith("**", i):
            out.append(".*")
            i += 2
        elif glob[i] == "*":
            out.append("[^/]*")
            i += 1
        elif glob[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(glob[i]))
            i += 1
    return re.compile("".join(out) + r"\Z")


class Globs:
    def __init__(self, globs: list[str]):
        self.globs = list(globs)
        self.regexes = [glob_regex(g) for g in globs]

    def match(self, path: str) -> bool:
        return any(r.match(path) for r in self.regexes)


# --------------------------------------------------------------------------- config

@dataclass
class Rule:
    kind: str
    id: str
    data: dict
    scope_in: Globs
    scope_except: Globs

    def applies(self, path: str) -> bool:
        return self.scope_in.match(path) and not self.scope_except.match(path)


def load_config(path: Path) -> tuple[Globs, list[Rule]]:
    try:
        data = tomllib.loads(path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        raise ConfigError(f"{path}: {e}") from e
    if data.get("version") != 1:
        raise ConfigError(f"{path}: version must be 1")
    unknown = set(data) - TOP_KEYS
    if unknown:
        raise ConfigError(f"{path}: unknown top-level keys {sorted(unknown)}")
    rules, seen = [], set()
    for kind, allowed in RULE_KINDS.items():
        for entry in data.get(kind, []):
            extra = set(entry) - allowed
            if extra:
                raise ConfigError(f"{path}: [[{kind}]] {entry.get('id')}: unknown keys {sorted(extra)}")
            rid = entry.get("id")
            if not rid or rid in seen:
                raise ConfigError(f"{path}: [[{kind}]] needs a unique id (got {rid!r})")
            seen.add(rid)
            rules.append(Rule(kind, rid, entry, Globs(entry.get("in", ["**"])), Globs(entry.get("except", []))))
    return Globs(data.get("non_production", [])), rules


# --------------------------------------------------------------------------- files

def iter_files(root: Path):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)
        for name in sorted(filenames):
            full = Path(dirpath) / name
            yield full.relative_to(root).as_posix(), full


def normalize(path: str) -> str | None:
    """Collapse `.`/`..`; None if it climbs out of the checkout."""
    parts: list[str] = []
    for part in PurePosixPath(path).parts:
        if part in ("", "."):
            continue
        if part == "..":
            if not parts:
                return None
            parts.pop()
        else:
            parts.append(part)
    return "/".join(parts)


class Lines:
    def __init__(self, text: str):
        self.starts = [0] + [i + 1 for i, c in enumerate(text) if c == "\n"]

    def line(self, offset: int) -> int:
        return bisect.bisect_right(self.starts, offset)


# --------------------------------------------------------------------------- Rust lexing

def scan_rust(src: str) -> tuple[str, list[bool]]:
    """Blank comments, string and char literals (newlines kept). Returns the blanked code
    and a mask that is True where the original character is code."""
    out = list(src)
    code = [True] * len(src)
    n, i = len(src), 0

    def blank(a: int, b: int) -> None:
        for k in range(a, min(b, n)):
            code[k] = False
            if out[k] != "\n":
                out[k] = " "

    def ident_char(c: str) -> bool:
        return c.isalnum() or c == "_"

    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c in "rb" and (i == 0 or not ident_char(src[i - 1])):
            m = re.compile(r'(?:br|r)(#*)"').match(src, i)
            if m:
                close = '"' + m.group(1)
                j = src.find(close, m.end())
                j = n if j < 0 else j + len(close)
                blank(i, j)
                i = j
            elif src.startswith('b"', i):
                j = _string_end(src, i + 2)
                blank(i, j)
                i = j
            elif src.startswith("b'", i):
                m2 = re.compile(r"b'(?:\\.[^']*|[^\\'])'").match(src, i)
                j = m2.end() if m2 else i + 2
                blank(i, j)
                i = j
            else:
                i += 1
        elif c == '"':
            j = _string_end(src, i + 1)
            blank(i, j)
            i = j
        elif c == "'":
            m = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'").match(src, i)
            if m:
                blank(i, m.end())
                i = m.end()
            else:
                i += 1  # a lifetime
        else:
            i += 1
    return "".join(out), code


def _string_end(src: str, i: int) -> int:
    n = len(src)
    while i < n:
        if src[i] == "\\":
            i += 2
        elif src[i] == '"':
            return i + 1
        else:
            i += 1
    return n


TEST_CFG = re.compile(
    r'#\s*(!?)\s*\[\s*cfg\s*\(\s*(?:'
    r'test'
    r'|feature\s*=\s*"test-support"'
    r'|any\s*\(\s*test\s*,\s*feature\s*=\s*"test-support"\s*\)'
    r'|any\s*\(\s*feature\s*=\s*"test-support"\s*,\s*test\s*\)'
    r')\s*\)\s*\]'
)
INNER_ATTR_PREFIX = re.compile(r"\A(?:\s|#!\s*\[[^\]]*\])*\Z")


def remove_test_items(src: str, code: str, mask: list[bool]) -> str | None:
    """Blank items gated by `#[cfg(test)]` or the test-support feature. None means the
    whole file is gated by an inner attribute."""
    out = list(code)
    for m in TEST_CFG.finditer(src):
        if not mask[m.start()]:
            continue
        if m.group(1) == "!":
            if INNER_ATTR_PREFIX.match(code[: m.start()]):
                return None
            continue
        j, n = m.end(), len(code)
        while j < n:  # skip further attributes, then blank the item through its body
            if code[j] == "#":
                bracket = code.find("[", j)
                if bracket < 0:
                    break
                j = _match_close(code, bracket, "[", "]")
                continue
            if code[j] == ";":
                j += 1
                break
            if code[j] == "{":
                j = _match_close(code, j, "{", "}")
                break
            j += 1
        for k in range(m.start(), min(j, n)):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def _match_close(text: str, i: int, open_c: str, close_c: str) -> int:
    depth = 0
    for j in range(i, len(text)):
        if text[j] == open_c:
            depth += 1
        elif text[j] == close_c:
            depth -= 1
            if depth == 0:
                return j + 1
    return len(text)


USE_STMT = re.compile(r"\buse\s+([^;]+);")
EXTERN_CRATE = re.compile(r"\bextern\s+crate\s+([A-Za-z_]\w*)")
QUALIFIED = re.compile(r"(?<![\w:])(?:::\s*)?([A-Za-z_]\w*(?:\s*::\s*[A-Za-z_]\w*)+)")


def expand_use(tree: str) -> list[str]:
    tree = re.sub(r"\s*(::|\{|\}|,)\s*", r"\1", tree.strip())
    tree = tree[2:] if tree.startswith("::") else tree
    return [p for p in _expand(tree) if p]


def _expand(tree: str) -> list[str]:
    brace = tree.find("{")
    if brace < 0:
        return [re.sub(r"\s+as\s+\w+$", "", tree).strip()]
    prefix = tree[:brace]
    inner = tree[brace + 1 : _match_close(tree, brace, "{", "}") - 1]
    parts, depth, start = [], 0, 0
    for k, ch in enumerate(inner):
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        elif ch == "," and depth == 0:
            parts.append(inner[start:k])
            start = k + 1
    parts.append(inner[start:])
    out = []
    for part in parts:
        part = part.strip()
        if not part:
            continue
        for leaf in _expand(part):
            out.append(prefix.rstrip(":") if leaf == "self" else prefix + leaf)
    return out


def path_hits(path: str, forbid: list[str]) -> str | None:
    for f in forbid:
        if path == f or path.startswith(f + "::"):
            return f
    return None


# --------------------------------------------------------------------------- checks

def check_rust_file(rel: str, src: str, rules: list[Rule], path_ban: list[Rule]) -> list[Violation]:
    code, mask = scan_rust(src)
    lines = Lines(src)
    found: list[Violation] = []

    for rule in path_ban:  # #[path] and include!-family reach other files
        for m in re.finditer(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]|\binclude(?:_str|_bytes)?\s*!\s*\(\s*"([^"]+)"', src):
            if not mask[m.start()]:
                continue
            target = normalize(str(PurePosixPath(rel).parent / (m.group(1) or m.group(2))))
            if target is not None and Globs(rule.data["to"]).match(target):
                found.append(Violation(rule.id, rel, lines.line(m.start()), f"reaches non-production path {target}"))

    live = remove_test_items(src, code, mask)
    if live is None:
        return found

    uses: list[tuple[int, str]] = []
    for m in USE_STMT.finditer(live):
        uses += [(m.start(), p) for p in expand_use(m.group(1))]
    for m in EXTERN_CRATE.finditer(live):
        uses.append((m.start(), m.group(1)))
    body = USE_STMT.sub(lambda m: re.sub(r"[^\n]", " ", m.group(0)), live)
    qualified = [(m.start(), re.sub(r"\s+", "", m.group(1))) for m in QUALIFIED.finditer(body)]

    for rule in rules:
        if not rule.applies(rel):
            continue
        if rule.kind == "rust_path":
            for offset, p in uses + qualified:
                hit = path_hits(p, rule.data["forbid"])
                if hit:
                    found.append(Violation(rule.id, rel, lines.line(offset), f"uses {p} (forbidden: {hit})"))
        elif rule.kind == "rust_ident":
            pattern = re.compile(r"\b(" + "|".join(map(re.escape, rule.data["forbid"])) + r")\b")
            for m in pattern.finditer(live):
                found.append(Violation(rule.id, rel, lines.line(m.start()), f"uses {m.group(1)}"))
    return found


def strip_js_comments(src: str) -> str:
    out, i, n = list(src), 0, len(src)
    while i < n:
        c = src[i]
        if c in "'\"`":
            j = i + 1
            while j < n and src[j] != c:
                j += 2 if src[j] == "\\" else 1
            i = j + 1
        elif src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            for k in range(i, j):
                out[k] = " "
            i = j
        elif src.startswith("/*", i):
            j = src.find("*/", i + 2)
            j = n if j < 0 else j + 2
            for k in range(i, j):
                if out[k] != "\n":
                    out[k] = " "
            i = j
        else:
            i += 1
    return "".join(out)


JS_SPECIFIERS = [
    re.compile(r"""\bimport\s+(?:type\s+)?(?:[^'"`;]*?\s+from\s+)?(['"])([^'"]+)\1"""),
    re.compile(r"""\bexport\s+(?:type\s+)?[^'"`;]*?\s+from\s+(['"])([^'"]+)\1"""),
    re.compile(r"""\bimport\s*\(\s*(['"])([^'"]+)\1"""),
    re.compile(r"""\brequire\s*\(\s*(['"])([^'"]+)\1"""),
]


def js_package(spec: str) -> str:
    parts = spec.split("/")
    return "/".join(parts[:2]) if spec.startswith("@") else parts[0]


def check_js_file(rel: str, src: str, rules: list[Rule], path_ban: list[Rule]) -> list[Violation]:
    text = strip_js_comments(src)
    lines = Lines(src)
    found: list[Violation] = []
    specs = sorted({(m.start(), m.group(2)) for rx in JS_SPECIFIERS for m in rx.finditer(text)})
    for offset, spec in specs:
        line = lines.line(offset)
        if spec.startswith("."):
            target = normalize(str(PurePosixPath(rel).parent / spec))
            for rule in path_ban:
                if target is not None and Globs(rule.data["to"]).match(target):
                    found.append(Violation(rule.id, rel, line, f"imports non-production path {target}"))
            continue
        pkg = js_package(spec)
        for rule in rules:
            if rule.kind == "js_import" and rule.applies(rel) and Globs(rule.data["packages"]).match(pkg):
                found.append(Violation(rule.id, rel, line, f"imports {spec}"))
    return found


def workspace_packages(root: Path) -> list[tuple[str, Path, dict]]:
    """(package name, manifest path, parsed manifest) for the root package and members."""
    root_manifest = root / "Cargo.toml"
    if not root_manifest.exists():
        return []
    root_data = tomllib.loads(root_manifest.read_text())
    manifests = []
    if "package" in root_data:
        manifests.append(root_manifest)
    ws = root_data.get("workspace", {})
    exclude = {normalize(e) for e in ws.get("exclude", [])}
    for member in ws.get("members", []):
        for d in sorted(root.glob(member)):
            rel = d.relative_to(root).as_posix()
            if rel in exclude or any(rel.startswith(e + "/") for e in exclude if e):
                continue
            if (d / "Cargo.toml").exists():
                manifests.append(d / "Cargo.toml")
    out = []
    for m in manifests:
        data = tomllib.loads(m.read_text())
        name = data.get("package", {}).get("name", m.parent.name)
        out.append((name, m, data))
    return out


def dependency_entries(data: dict):
    """Yield (key, spec) for normal and build dependencies, including target tables."""
    tables = [data]
    tables += list(data.get("target", {}).values())
    for t in tables:
        for section in ("dependencies", "build-dependencies"):
            for key, spec in t.get(section, {}).items():
                yield key, spec


def check_cargo(root: Path, non_prod: Globs, rules: list[Rule], path_ban: list[Rule]) -> list[Violation]:
    found: list[Violation] = []
    root_ws_deps = {}
    if (root / "Cargo.toml").exists():
        root_ws_deps = tomllib.loads((root / "Cargo.toml").read_text()).get("workspace", {}).get("dependencies", {})
    for pkg, manifest, data in workspace_packages(root):
        rel_manifest = manifest.relative_to(root).as_posix()
        if non_prod.match(rel_manifest):
            continue
        text = manifest.read_text()
        lines = text.splitlines()
        for key, spec in dependency_entries(data):
            base = manifest.parent
            if isinstance(spec, dict) and spec.get("workspace") is True:
                ws_spec = root_ws_deps.get(key, {})
                spec = ws_spec if isinstance(ws_spec, dict) else {}
                base = root
            real = spec.get("package", key) if isinstance(spec, dict) else key
            line = next((k + 1 for k, l in enumerate(lines)
                         if re.match(rf'\s*(\[[^\]]*[.\s])?{re.escape(key)}(?![\w-])', l)), 1)
            for rule in rules:
                if rule.kind == "crate_ban" and real in rule.data["crates"]:
                    found.append(Violation(rule.id, rel_manifest, line, f"{pkg} depends on {real}"))
                if rule.kind == "crate_scope" and real in rule.data["crates"] and pkg not in rule.data["packages"]:
                    found.append(Violation(rule.id, rel_manifest, line,
                                           f"{pkg} depends on {real}; only {', '.join(rule.data['packages'])} may"))
            if isinstance(spec, dict) and "path" in spec:
                target = normalize(os.path.relpath(base / spec["path"], root).replace(os.sep, "/"))
                for rule in path_ban:
                    if target is not None and (Globs(rule.data["to"]).match(target)
                                               or Globs(rule.data["to"]).match(target + "/Cargo.toml")):
                        found.append(Violation(rule.id, rel_manifest, line, f"{pkg} depends on path {target}"))
    return found


def run(root: Path, config: Path) -> list[Violation]:
    non_prod, rules = load_config(config)
    path_ban = [r for r in rules if r.kind == "path_ban"]
    found = check_cargo(root, non_prod, rules, path_ban)
    for rel, full in iter_files(root):
        if non_prod.match(rel):
            continue
        ext = PurePosixPath(rel).suffix
        for rule in rules:
            if rule.kind == "source_language" and rule.applies(rel) and ext in rule.data["forbid_extensions"]:
                found.append(Violation(rule.id, rel, 1, f"{ext} source file"))
        if ext in RUST_EXT:
            found += check_rust_file(rel, full.read_text(errors="replace"), rules, path_ban)
        elif ext in JS_EXT:
            found += check_js_file(rel, full.read_text(errors="replace"), rules, path_ban)
    return sorted(set(found), key=lambda v: (v.path, v.line, v.rule, v.message))


def default_root() -> Path:
    try:
        top = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True)
        return Path(top.stdout.strip())
    except (OSError, subprocess.CalledProcessError):
        return Path.cwd()


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", type=Path, default=None, help="checkout root (default: git toplevel)")
    ap.add_argument("--config", type=Path, default=None, help="rules (default: tools/arch-guard/guard.toml)")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)
    root = (args.root or default_root()).resolve()
    config = args.config or root / "tools" / "arch-guard" / "guard.toml"
    try:
        found = run(root, config)
    except ConfigError as e:
        print(f"arch-guard: {e}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps({"violations": [asdict(v) for v in found]}, indent=2))
    else:
        for v in found:
            print(f"{v.path}:{v.line}: {v.rule}: {v.message}")
        print(f"arch-guard: {len(found)} violation(s)" if found else "arch-guard: clean")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
