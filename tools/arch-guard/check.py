#!/usr/bin/env python3
"""Architecture guard: checks the rules .trellis/rules.toml can't express.

    python3 -I tools/arch-guard/check.py            # human output, exit 1 on violations
    python3 -I tools/arch-guard/check.py --json     # machine output

Rules live in tools/arch-guard/guard.toml; SPEC.md ("Repository layout and architecture
rules") says which rows of the architecture table this covers. It reads files only: it
never runs Cargo, npm or the code it checks. Standard library only.

It tokenizes Rust and TypeScript/JavaScript, but it is not a compiler. What it does:
- Rust: comments and literals are tokens, never code; raw identifiers (`r#fs`) are
  normalized; string literals (normal, raw, byte) are decoded where a path argument is
  read. Items under a test gate are removed: the attributes in TEST_ATTRIBUTES (`#[test]`,
  `#[tokio::test(...)]`; any other attribute, even `#[x::test]`, leaves code in
  production), and `#[cfg(...)]` predicates that are test-only (`test`,
  `feature = "<test feature>"`, `any` of those, `all` containing one). `#![cfg(...)]` at
  the top of a file or block gates all of it. Item boundaries track brackets, and angle
  brackets in type position (`Result<(), File>`, `Marker<{ 1 }>`).
  Import roots map back to the real crate: Cargo key renames (including
  `workspace = true`), an in-checkout dependency's `[lib] name`, and
  `extern crate x as y;` within its own block.
- Frontend: static `import`/`export ... from` (string-named bindings included),
  `import()` and `require()`/`require?.()` are read from tokens, with string escapes
  decoded; method definitions named `import`/`require` aren't loads. Strings, templates
  and regex literals are never scanned as code; regex-versus-division follows the
  previous token with context (control-statement `)`, block `}`, postfix `++`, `x!`),
  and line comments end at every ECMAScript line terminator. A non-literal
  `import()`/`require()` argument is reported by the `js_literal_imports` rule, and an
  import/export declaration this grammar can't follow is reported as `guard-parse`.
- Cargo: workspace members are the root `members` globs (minus `exclude`) plus every path
  dependency inside the checkout, recursively, as Cargo does. Both spellings of the
  dependency tables (`build-dependencies` and edition-2021 `build_dependencies`) count.
- Paths: path bans check a reach as spelled and, through symlinks, where it really lands
  inside the checkout; a production path that is a symlink into a banned path is flagged.
- Text it can't tokenize (an unterminated string or comment) is reported as `guard-parse`,
  never passed as clean. A `skip` glob that would hide files inside a package's `src/` or
  a rule's scope, or a directory above one, is reported as `guard-skip`.

Known limits, by design:
- Rust: macros are not expanded; re-exports, glob imports of a parent (`use std::*;`)
  and aliases of a crate root (`use std as s;`) are not followed. File ownership by
  package is by containing directory, not by the module tree.
- Frontend: JSX tags are handled: the `/` of a closing tag (`</p>`, `</>`) and of a
  self-closing tag (`<Foo />`, `<Foo {...x} />`) is punctuation. The cost is that in plain
  JS `a </re/` and `{ } />/` read as JSX, which can only add references, never hide one.
  JSX text is still not modeled; a `.tsx` file whose JSX text holds an unpaired quote
  may be reported as `guard-parse`, so keep quotes, backticks and slashes out of JSX text.
- Cargo: `[patch]`, `[replace]` and dependencies added by build scripts are not seen. A
  registry crate's library name is assumed to be its package name.
- Frontend: a line comment is located by `\n` line numbers; text after U+2028/U+2029 is
  reported on the same line.
- Identifier rules (`rust_ident`) reserve names: they match the spelling, so a local
  type with a reserved name is flagged too. They are a conservative list, not proof that
  no other API does the same thing.
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
from dataclasses import dataclass, asdict, field
from pathlib import Path, PurePosixPath

RUST_EXT = {".rs"}
JS_EXT = {".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"}
RULE_KINDS = {
    "crate_ban": {"id", "crates", "reason"},
    "crate_scope": {"id", "crates", "packages", "reason"},
    "rust_path": {"id", "in", "except", "forbid", "reason"},
    "rust_ident": {"id", "in", "except", "forbid", "reason"},
    "js_import": {"id", "in", "except", "packages", "reason"},
    "js_literal_imports": {"id", "in", "except", "reason"},
    "source_language": {"id", "in", "except", "forbid_extensions", "reason"},
    "path_ban": {"id", "to", "reason"},
}
TOP_KEYS = {"version", "non_production", "skip", "test_features"} | set(RULE_KINDS)
PARSE_RULE = "guard-parse"
SKIP_RULE = "guard-skip"
PROD_DEP_SECTIONS = ("dependencies", "build-dependencies", "build_dependencies")
DEV_DEP_SECTIONS = ("dev-dependencies", "dev_dependencies")


@dataclass(frozen=True)
class Violation:
    rule: str
    path: str
    line: int
    message: str


class ConfigError(Exception):
    pass


class LexError(Exception):
    def __init__(self, offset: int, message: str):
        super().__init__(message)
        self.offset = offset
        self.message = message


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

    def reaches(self, target: str | None) -> bool:
        """A reach of `target` or of the directory `target` (its index, its manifest)."""
        return target is not None and (self.match(target) or self.match(target + "/"))

    def prefixes(self) -> list[str]:
        """Literal directory prefixes of the globs (`a/b/**` -> `a/b/`)."""
        out = []
        for g in self.globs:
            lit = re.split(r"[*?]", g, maxsplit=1)[0]
            lit = lit[: lit.rfind("/") + 1]
            if lit:
                out.append(lit)
        return out


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


@dataclass
class Config:
    non_production: Globs
    skip: Globs
    test_features: set[str]
    rules: list[Rule]

    def of(self, kind: str) -> list[Rule]:
        return [r for r in self.rules if r.kind == kind]


def load_config(path: Path) -> Config:
    try:
        data = tomllib.loads(path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        raise ConfigError(f"{path}: {e}") from e
    if data.get("version") != 1:
        raise ConfigError(f"{path}: version must be 1")
    unknown = set(data) - TOP_KEYS
    if unknown:
        raise ConfigError(f"{path}: unknown top-level keys {sorted(unknown)}")
    rules, seen = [], {PARSE_RULE, SKIP_RULE}
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
    return Config(
        Globs(data.get("non_production", [])),
        Globs(data.get("skip", [])),
        set(data.get("test_features", [])),
        rules,
    )


# --------------------------------------------------------------------------- files

def walk(root: Path, skip: Globs) -> tuple[list[tuple[str, Path]], list[str], list[str]]:
    """(checkout-relative path, full path) of every file, the directories skipped, and
    every symlink (to a file or a directory; directory links aren't followed)."""
    files, skipped, links = [], [], []
    for dirpath, dirnames, filenames in os.walk(root):
        rel_dir = Path(dirpath).relative_to(root).as_posix()
        rel_dir = "" if rel_dir == "." else rel_dir
        keep = []
        for d in sorted(dirnames):
            rel = f"{rel_dir}/{d}" if rel_dir else d
            if d == ".git":
                continue
            if skip.match(rel + "/"):
                skipped.append(rel)
                continue
            if (Path(dirpath) / d).is_symlink():
                links.append(rel)
            keep.append(d)
        dirnames[:] = keep
        for name in sorted(filenames):
            full = Path(dirpath) / name
            rel = full.relative_to(root).as_posix()
            if full.is_symlink():
                links.append(rel)
            files.append((rel, full))
    return files, skipped, links


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


def reach_targets(root: Path | None, target: str | None) -> list[str]:
    """`target` as spelled and, if a symlink makes it resolve elsewhere inside the
    checkout, its canonical path too. A target outside the checkout yields nothing more."""
    if target is None:
        return []
    out = [target]
    if root is not None:
        real_root = os.path.realpath(root)
        real = os.path.realpath(os.path.join(root, target))
        if real == real_root or real.startswith(real_root + os.sep):
            canonical = normalize(os.path.relpath(real, real_root).replace(os.sep, "/"))
            if canonical is not None and canonical != target:
                out.append(canonical)
    return out


class Lines:
    def __init__(self, text: str):
        self.starts = [0] + [i + 1 for i, c in enumerate(text) if c == "\n"]

    def line(self, offset: int) -> int:
        return bisect.bisect_right(self.starts, offset)


@dataclass(frozen=True, slots=True)
class Tok:
    kind: str  # ident punct str char lifetime num | regex template template_expr template_tail
    text: str  # identifier (normalized), punctuation, or a literal's decoded value
    start: int

    def is_(self, kind: str, text: str | None = None) -> bool:
        return self.kind == kind and (text is None or self.text == text)


def _group_end(toks: list[Tok], i: int) -> int:
    """toks[i] opens a group; the index after its matching close."""
    depth = 0
    for j in range(i, len(toks)):
        t = toks[j]
        if t.kind == "punct" and t.text in ("(", "[", "{"):
            depth += 1
        elif t.kind == "punct" and t.text in (")", "]", "}"):
            depth -= 1
            if depth == 0:
                return j + 1
    return len(toks)


# --------------------------------------------------------------------------- Rust lexing

_R_RAW_STR = re.compile(r'(?:br|cr|r)(#*)"')
_R_RAW_IDENT = re.compile(r"r#([^\W\d]\w*)")
_R_IDENT = re.compile(r"[^\W\d]\w*")
_R_NUM = re.compile(r"\d\w*(?:\.\d\w*)?")
_R_CHAR = re.compile(r"b?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n\r\t])'")
_R_LIFETIME = re.compile(r"'[^\W\d]\w*")
_R_ESC = re.compile(r"\\(?:u\{([0-9a-fA-F_]+)\}|x([0-9a-fA-F]{2})|(\r?\n\s*)|(.))", re.S)
_SIMPLE_ESC = {"n": "\n", "r": "\r", "t": "\t", "0": "\0"}


def _chr(hexdigits: str, fallback: str) -> str:
    try:
        return chr(int(hexdigits.replace("_", ""), 16))
    except ValueError:
        return fallback


def _rust_unescape(body: str) -> str:
    def sub(m: re.Match[str]) -> str:
        if m.group(1) is not None:
            return _chr(m.group(1), m.group(0))
        if m.group(2) is not None:
            return _chr(m.group(2), m.group(0))
        if m.group(3) is not None:
            return ""  # line continuation
        return _SIMPLE_ESC.get(m.group(4), m.group(4))
    return _R_ESC.sub(sub, body)


def lex_rust(src: str) -> list[Tok]:
    toks: list[Tok] = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c.isspace():
            i += 1
        elif src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
        elif src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            if depth:
                raise LexError(i, "unterminated block comment")
            i = j
        elif m := _R_RAW_STR.match(src, i):
            close = '"' + m.group(1)
            j = src.find(close, m.end())
            if j < 0:
                raise LexError(i, "unterminated raw string")
            toks.append(Tok("str", src[m.end():j], i))
            i = j + len(close)
        elif m := _R_RAW_IDENT.match(src, i):
            toks.append(Tok("ident", m.group(1), i))
            i = m.end()
        elif c == '"' or (c in "bc" and src.startswith('"', i + 1)):
            q = i if c == '"' else i + 1
            j = q + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            if j >= n:
                raise LexError(i, "unterminated string")
            toks.append(Tok("str", _rust_unescape(src[q + 1:j]), i))
            i = j + 1
        elif c == "'" or (c == "b" and src.startswith("'", i + 1)):
            if m := _R_CHAR.match(src, i):
                toks.append(Tok("char", m.group(0), i))
            elif c == "'" and (m := _R_LIFETIME.match(src, i)):
                toks.append(Tok("lifetime", m.group(0), i))
            else:
                raise LexError(i, "unrecognized quote")
            i = m.end()
        elif m := _R_IDENT.match(src, i):
            toks.append(Tok("ident", m.group(0), i))
            i = m.end()
        elif m := _R_NUM.match(src, i):
            toks.append(Tok("num", m.group(0), i))
            i = m.end()
        elif src.startswith(("::", "->", "=>"), i):
            toks.append(Tok("punct", src[i:i + 2], i))
            i += 2
        else:
            toks.append(Tok("punct", c, i))
            i += 1
    return toks


# --------------------------------------------------------------------------- Rust test gates

def _attr(toks: list[Tok], i: int) -> tuple[bool, list[Tok], int] | None:
    """An attribute starting at toks[i]: (inner, content tokens, index after it)."""
    if not toks[i].is_("punct", "#"):
        return None
    j, inner = i + 1, False
    if j < len(toks) and toks[j].is_("punct", "!"):
        inner, j = True, j + 1
    if j < len(toks) and toks[j].is_("punct", "["):
        end = _group_end(toks, j)
        return inner, toks[j + 1:end - 1], end
    return None


def _parse_cfg(ts: list[Tok], k: int):
    t = ts[k]
    if t.kind != "ident":
        raise ValueError("cfg predicate")
    k += 1
    if k < len(ts) and ts[k].is_("punct", "="):
        if ts[k + 1].kind != "str":
            raise ValueError("cfg value")
        return ("kv", t.text, ts[k + 1].text), k + 2
    if k < len(ts) and ts[k].is_("punct", "("):
        kids, k = [], k + 1
        while not ts[k].is_("punct", ")"):
            node, k = _parse_cfg(ts, k)
            kids.append(node)
            if ts[k].is_("punct", ","):
                k += 1
            elif not ts[k].is_("punct", ")"):
                raise ValueError("cfg list")
        return ("call", t.text, kids), k + 1
    return ("flag", t.text), k


def _test_only(node, features: set[str]) -> bool:
    if node[0] == "flag":
        return node[1] == "test"
    if node[0] == "kv":
        return node[1] == "feature" and node[2] in features
    name, kids = node[1], node[2]
    if name == "any":
        return bool(kids) and all(_test_only(c, features) for c in kids)
    if name == "all":
        return any(_test_only(c, features) for c in kids)
    return False  # not(...) and anything unknown: production


# Attributes that compile their item only into test builds. Any other attribute, even
# one whose path ends in `test`, leaves the item in production.
TEST_ATTRIBUTES = {"test", "tokio::test"}


def _is_test_attr(content: list[Tok], features: set[str]) -> bool:
    k = 0
    while k < len(content) and (content[k].kind == "ident" or content[k].is_("punct", "::")):
        k += 1
    path = "".join(t.text for t in content[:k])
    rest = content[k:]
    if path in TEST_ATTRIBUTES and (not rest or (rest[0].is_("punct", "(") and _group_end(content, k) == len(content))):
        return True  # #[test], #[tokio::test], #[tokio::test(flavor = "multi_thread")]
    if len(content) >= 3 and content[0].is_("ident", "cfg") and content[1].is_("punct", "("):
        try:
            node, k = _parse_cfg(content, 2)
            if k < len(content) and content[k].is_("punct", ","):
                k += 1
            return k == len(content) - 1 and content[k].is_("punct", ")") and _test_only(node, features)
        except (ValueError, IndexError):
            return False
    return False


_ITEM_KEYWORDS = {"fn", "mod", "impl", "trait", "struct", "enum", "union", "type", "const", "static",
                  "extern", "use", "macro_rules", "let", "unsafe", "async", "auto"}
_SEMI_KEYWORDS = {"const", "static", "let", "type", "use"}


def _item_end(toks: list[Tok], k: int) -> int:
    """The index after the item (or field, variant, arm, statement) that starts at k.

    In type position (an item's header before `=` or its body, or a field) `<` and `>`
    nest too, so `Result<(), File>` and `Marker<{ 1 }>` stay inside the item."""
    n = len(toks)
    while k < n and (a := _attr(toks, k)) and not a[0]:
        k = a[2]
    j = k
    while j < n and toks[j].is_("ident", "pub"):
        j += 1
        if j < n and toks[j].is_("punct", "("):
            j = _group_end(toks, j)
    first = toks[j].text if j < n and toks[j].kind == "ident" else None
    after = toks[j + 1] if j + 1 < n else None
    keyword = first in _ITEM_KEYWORDS
    field = not keyword and after is not None and after.is_("punct", ":")
    types = keyword or field  # angle brackets nest until an initializer starts
    to_semicolon = first in _SEMI_KEYWORDS and not (
        first == "const" and after is not None and after.kind == "ident" and after.text in {"fn", "unsafe", "async", "extern"})
    depth = angle = 0
    for m in range(k, n):
        t = toks[m]
        if t.kind != "punct":
            continue
        if types and t.text == "<":
            angle += 1
        elif types and t.text == ">":
            angle = max(0, angle - 1)
        elif t.text in ("(", "[", "{"):
            if t.text == "{" and depth == 0 and angle == 0 and not to_semicolon:
                return _group_end(toks, m)
            depth += 1
        elif t.text in (")", "]", "}"):
            if depth == 0:
                return m  # the close of an enclosing group
            depth -= 1
        elif depth == 0 and angle == 0:
            if t.text == ";":
                return m + 1
            if t.text == "," and not keyword:
                return m + 1
            if t.text == "=" and not field:
                types = False  # an initializer: `<` is a comparison from here
    return n


def live_rust_tokens(toks: list[Tok], features: set[str]) -> list[Tok] | None:
    """The tokens outside test-gated items. None: the whole file is test-gated."""
    n = len(toks)
    dead = [False] * n

    def kill(a: int, b: int) -> None:
        for k in range(a, min(b, n)):
            dead[k] = True

    head: int | None = -1  # -1: top of file; k: just inside the `{` at k; None: neither
    i = 0
    while i < n:
        a = _attr(toks, i)
        if a is None:
            head = i if toks[i].is_("punct", "{") else None
            i += 1
            continue
        inner, content, end = a
        if inner:
            if _is_test_attr(content, features):
                if head == -1:
                    return None
                if head is not None:
                    kill(head, _group_end(toks, head))
            i = end
            continue
        head = None
        if _is_test_attr(content, features):
            stop = _item_end(toks, end)
            kill(i, stop)
            i = max(end, stop)
        else:
            i = end
    return [t for k, t in enumerate(toks) if not dead[k]]


# --------------------------------------------------------------------------- Rust paths

def expand_use(tree: str) -> list[str]:
    tree = re.sub(r"\s*(::|\{|\}|,)\s*", r"\1", tree.strip())
    tree = tree[2:] if tree.startswith("::") else tree
    return [p for p in _expand(tree) if p]


def _expand(tree: str) -> list[str]:
    brace = tree.find("{")
    if brace < 0:
        return [re.sub(r"\s+as\s+\w+$", "", tree).strip()]
    prefix = tree[:brace]
    depth, close = 0, len(tree)
    for k in range(brace, len(tree)):
        depth += {"{": 1, "}": -1}.get(tree[k], 0)
        if depth == 0:
            close = k
            break
    inner = tree[brace + 1:close]
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


def _rebind(path: str, bindings: dict[str, str]) -> str:
    first, sep, rest = path.partition("::")
    return bindings[first] + sep + rest if first in bindings else path


_INCLUDES = {"include", "include_str", "include_bytes"}


def check_rust_file(rel: str, src: str, cfg: Config, bindings: dict[str, str], root: Path | None = None) -> list[Violation]:
    lines = Lines(src)
    try:
        toks = lex_rust(src)
    except LexError as e:
        return [Violation(PARSE_RULE, rel, lines.line(e.offset), f"cannot tokenize Rust: {e.message}")]
    live = live_rust_tokens(toks, cfg.test_features)
    if live is None:
        return []
    found: list[Violation] = []
    n = len(live)

    def tok(k: int) -> Tok:
        return live[k] if k < n else Tok("eof", "", len(src))

    # #[path = "..."] and include!-family macros reach other files.
    for k, t in enumerate(live):
        arg = None
        if (t.is_("punct", "#") and tok(k + 1).is_("punct", "[") and tok(k + 2).is_("ident", "path")
                and tok(k + 3).is_("punct", "=") and tok(k + 4).kind == "str"):
            arg = tok(k + 4).text
        elif (t.kind == "ident" and t.text in _INCLUDES and tok(k + 1).is_("punct", "!")
              and tok(k + 2).is_("punct", "(") and tok(k + 3).kind == "str"):
            arg = tok(k + 3).text
        if arg is None:
            continue
        for target in reach_targets(root, normalize(str(PurePosixPath(rel).parent / arg))):
            for rule in cfg.of("path_ban"):
                if Globs(rule.data["to"]).reaches(target):
                    found.append(Violation(rule.id, rel, lines.line(t.start), f"reaches non-production path {target}"))

    # The enclosing `{` of every token, so an `extern crate x as y;` alias applies only
    # inside its own block or module, and the innermost alias wins.
    parent, stack = [-1] * n, []
    for k, t in enumerate(live):
        parent[k] = stack[-1] if stack else -1
        if t.is_("punct", "{"):
            stack.append(k)
        elif t.is_("punct", "}") and stack:
            stack.pop()
    aliases: dict[int, dict[str, str]] = {}  # enclosing `{` (-1: file) -> alias -> crate

    paths: list[tuple[int, int, str]] = []  # (token index, offset, path)
    k = 0
    while k < n:
        t = live[k]
        if t.is_("ident", "use") and not tok(k + 1).is_("punct", "<"):
            j = k + 1
            while j < n and not live[j].is_("punct", ";"):
                j += 1
            text = "".join(" as " if p.is_("ident", "as") else p.text for p in live[k + 1:j])
            paths += [(k, t.start, p) for p in expand_use(text)]
            k = j + 1
            continue
        if t.is_("ident", "extern") and tok(k + 1).is_("ident", "crate") and tok(k + 2).kind == "ident":
            name = tok(k + 2).text
            paths.append((k, t.start, name))
            if tok(k + 3).is_("ident", "as") and tok(k + 4).kind == "ident":
                aliases.setdefault(parent[k], {})[tok(k + 4).text] = name
            k += 3
            continue
        lead = t.is_("punct", "::") and tok(k + 1).kind == "ident"
        if (t.kind == "ident" or lead) and not (k and live[k - 1].is_("punct", "::")):
            j = k + 1 if lead else k
            segs = [live[j].text]
            j += 1
            while tok(j).is_("punct", "::") and tok(j + 1).kind == "ident":
                segs.append(live[j + 1].text)
                j += 2
            if len(segs) > 1:
                paths.append((k, t.start, "::".join(segs)))
            k = j
            continue
        k += 1

    def visible(k: int) -> dict[str, str]:
        chain, b = [], parent[k]
        while True:
            chain.append(b)
            if b == -1:
                break
            b = parent[b]
        out = dict(bindings)
        for scope in reversed(chain):  # outermost first, so inner aliases win
            for alias, crate in aliases.get(scope, {}).items():
                out[alias] = bindings.get(crate, crate)
        return out

    paths = [(off, _rebind(p, visible(k))) for k, off, p in paths]

    for rule in cfg.rules:
        if not rule.applies(rel):
            continue
        if rule.kind == "rust_path":
            for offset, p in paths:
                hit = path_hits(p, rule.data["forbid"])
                if hit:
                    found.append(Violation(rule.id, rel, lines.line(offset), f"uses {p} (forbidden: {hit})"))
        elif rule.kind == "rust_ident":
            reserved = set(rule.data["forbid"])
            for t in live:
                if t.kind == "ident" and t.text in reserved:
                    found.append(Violation(rule.id, rel, lines.line(t.start), f"uses {t.text}"))
    return found


# --------------------------------------------------------------------------- JS/TS lexing

_J_UESC = r"\\u(?:\{[0-9a-fA-F]+\}|[0-9a-fA-F]{4})"
_J_IDENT = re.compile(rf"(?:[^\W\d]|\$|{_J_UESC})(?:[\w$]|{_J_UESC})*")
_J_NUM = re.compile(r"\.?\d[\w.]*")
_J_ESC = re.compile(r"\\(?:u\{([0-9a-fA-F]+)\}|u([0-9a-fA-F]{4})|x([0-9a-fA-F]{2})|(\r\n|\r|\n|\u2028|\u2029)|(.))", re.S)
_J_SIMPLE_ESC = {"n": "\n", "r": "\r", "t": "\t", "b": "\b", "f": "\f", "v": "\v", "0": "\0"}
_REGEX_AFTER_WORD = {"return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "throw",
                     "case", "do", "else", "yield", "await"}


def _js_unescape(s: str) -> str:
    def sub(m: re.Match[str]) -> str:
        for g in (1, 2, 3):
            if m.group(g) is not None:
                return _chr(m.group(g), m.group(0))
        if m.group(4) is not None:
            return ""  # line continuation
        return _J_SIMPLE_ESC.get(m.group(5), m.group(5))
    return _J_ESC.sub(sub, s) if "\\" in s else s


_J_LINE_BREAKS = "\n\r  "
_METHOD_PREFIXES = {"async", "static", "get", "set", "public", "private", "protected", "readonly", "override", "accessor"}


def _line_end(src: str, i: int) -> int:
    """The index of the next ECMAScript line terminator at or after i (or len(src))."""
    ends = [j for j in (src.find(c, i) for c in _J_LINE_BREAKS) if j >= 0]
    return min(ends) if ends else len(src)


def _jsx_tag_slash(src: str, i: int, toks: list[Tok]) -> bool:
    """Whether the `/` at `i` belongs to a JSX tag, not a regex literal: the closing
    tag's `</` (the `<` token touches the `/`), or a self-closing `/>` after the `}` of
    a `{...props}` attribute. Approximation: in plain JS `a </re/` and `{ } />/` read
    as JSX here; the `/` is then punctuation, so the regex body is lexed as code and can
    only add references, never hide one."""
    if not toks:
        return False
    prev = toks[-1]
    if prev.kind != "punct":
        return False
    if prev.text == "<" and prev.start == i - 1:
        return True
    return prev.text == "}" and src.startswith("/>", i)


def lex_js(src: str) -> list[Tok]:
    """Tokens of a JS/TS module. Whether `/` starts a regex depends on whether the
    previous token ends an expression, which takes some context: `)` after `if (...)`
    and `}` after a block don't, postfix `x++` and TypeScript's `x!` do."""
    toks: list[Tok] = []
    ends_expr: set[int] = set()  # token indices after which `/` is division
    braces: list[str] = []  # "block", "object" or "template", innermost last
    parens: list[bool] = []  # whether each open `(` is a control-statement head
    i, n = 0, len(src)
    if src.startswith("#!"):
        i = _line_end(src, 0)

    def template_text(start: int, opened: int) -> tuple[int, str, bool]:
        j = start
        while j < n:
            if src[j] == "\\":
                j += 2
            elif src[j] == "`":
                return j + 1, src[start:j], True
            elif src.startswith("${", j):
                return j + 2, src[start:j], False
            else:
                j += 1
        raise LexError(opened, "unterminated template literal")

    def last_ends_expr() -> bool:
        return bool(toks) and len(toks) - 1 in ends_expr

    def push(kind: str, text: str, at: int, value_end: bool) -> None:
        if value_end:
            ends_expr.add(len(toks))
        toks.append(Tok(kind, text, at))

    while i < n:
        c = src[i]
        if c.isspace():
            i += 1
        elif src.startswith("//", i):
            i = _line_end(src, i)
        elif src.startswith("/*", i):
            j = src.find("*/", i + 2)
            if j < 0:
                raise LexError(i, "unterminated block comment")
            i = j + 2
        elif c in "'\"":
            j = i + 1
            while j < n and src[j] != c:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] in "\r\n":
                    raise LexError(i, "unterminated string")
                j += 1
            if j >= n:
                raise LexError(i, "unterminated string")
            push("str", _js_unescape(src[i + 1:j]), i, True)
            i = j + 1
        elif c == "`":
            end, text, closed = template_text(i + 1, i)
            if closed:
                push("template", _js_unescape(text), i, True)
            else:
                push("template_expr", "", i, False)
                braces.append("template")
            i = end
        elif c == "/" and _jsx_tag_slash(src, i, toks):
            push("punct", c, i, False)
            i += 1
        elif c == "/" and not last_ends_expr():
            j, in_class = i + 1, False
            while j < n:
                ch = src[j]
                if ch == "\\":
                    j += 2
                    continue
                if ch in _J_LINE_BREAKS:
                    raise LexError(i, "unterminated regular expression")
                if ch == "[":
                    in_class = True
                elif ch == "]":
                    in_class = False
                elif ch == "/" and not in_class:
                    break
                j += 1
            if j >= n:
                raise LexError(i, "unterminated regular expression")
            j += 1
            while j < n and (src[j].isalnum() or src[j] in "_$"):
                j += 1
            push("regex", src[i:j], i, True)
            i = j
        elif m := _J_IDENT.match(src, i):
            word = _js_unescape(m.group(0))
            push("ident", word, i, word not in _REGEX_AFTER_WORD)
            i = m.end()
        elif (c.isdigit() or (c == "." and i + 1 < n and src[i + 1].isdigit())) and (m := _J_NUM.match(src, i)):
            push("num", m.group(0), i, True)
            i = m.end()
        elif c == "{":
            prev = toks[-1] if toks else None
            expression = prev is not None and (
                (prev.kind == "punct" and prev.text not in (")", ";", "{", "}", "=>"))
                or (prev.kind == "ident" and prev.text in _REGEX_AFTER_WORD))
            braces.append("object" if expression else "block")
            push("punct", c, i, False)
            i += 1
        elif c == "}":
            kind = braces.pop() if braces else "block"
            if kind == "template":
                end, _, closed = template_text(i + 1, i)
                if closed:
                    push("template_tail", "", i, True)
                else:
                    braces.append("template")
                i = end
                continue
            push("punct", c, i, kind == "object")
            i += 1
        elif c == "(":
            prev = toks[-1] if toks else None
            before = toks[-2] if len(toks) > 1 else None
            control = prev is not None and prev.kind == "ident" and (
                prev.text in ("if", "while", "for", "with")
                or (prev.text == "await" and before is not None and before.is_("ident", "for")))
            parens.append(control)
            push("punct", c, i, False)
            i += 1
        elif c == ")":
            control = parens.pop() if parens else False
            push("punct", c, i, not control)
            i += 1
        elif c == "]":
            push("punct", c, i, True)
            i += 1
        elif src.startswith(("++", "--"), i):
            push("punct", src[i:i + 2], i, last_ends_expr())  # postfix ends an expression
            i += 2
        elif c == "!" and not src.startswith("!=", i) and last_ends_expr():
            push("punct", c, i, True)  # TypeScript non-null assertion: `x!`
            i += 1
        elif src.startswith("=>", i):
            push("punct", "=>", i, False)
            i += 2
        elif src.startswith("?.", i) and not (i + 2 < n and src[i + 2].isdigit()):
            push("punct", "?.", i, False)
            i += 2
        else:
            push("punct", c, i, False)
            i += 1
    return toks


def js_imports(toks: list[Tok]) -> list[tuple[int, str, str | None]]:
    """(offset, kind, specifier) for each module load. kind is "literal", "computed"
    (an `import()`/`require()` argument that isn't a literal) or "unsupported" (an
    `import`/`export ... from` declaration this grammar doesn't follow)."""
    out: list[tuple[int, str, str | None]] = []
    n = len(toks)

    def tok(k: int) -> Tok:
        return toks[k] if k < n else Tok("eof", "", -1)

    def binding_name(k: int) -> int:  # `name` or `"string name"`
        return k + 1 if tok(k).kind in ("ident", "str") else k

    for i, t in enumerate(toks):
        if t.kind != "ident" or t.text not in ("import", "export", "require"):
            continue
        prev = toks[i - 1] if i else None
        if prev is not None and ((prev.kind == "punct" and prev.text in (".", "?.")) or prev.is_("ident", "function")):
            continue  # x.import, x.require, function require() {}
        nxt = tok(i + 1)

        call = None
        if t.text in ("import", "require") and nxt.is_("punct", "("):
            call = i + 1
        elif t.text == "require" and nxt.is_("punct", "?.") and tok(i + 2).is_("punct", "("):
            call = i + 2  # require?.("x")
        if call is not None:
            after_args = tok(_group_end(toks, call))
            member_head = prev is None or (prev.kind == "punct" and prev.text in ("{", "}", ";", ",", "*")) or (
                prev.kind == "ident" and prev.text in _METHOD_PREFIXES)
            if after_args.is_("punct", "{") or (after_args.is_("punct", ":") and member_head):
                continue  # a method definition: import(value) { ... }
            arg, after = tok(call + 1), tok(call + 2)
            if arg.kind in ("str", "template") and after.kind == "punct" and after.text in (")", ","):
                out.append((t.start, "literal", arg.text))
            else:
                out.append((t.start, "computed", None))
            continue
        if t.text == "require":
            continue

        j = i + 1
        if t.text == "import":
            if nxt.kind == "eof" or (nxt.kind == "punct" and nxt.text not in ("{", "*")):
                continue  # import.meta, { import: x }
            if nxt.kind == "str":
                out.append((t.start, "literal", nxt.text))
                continue
            if tok(j).is_("ident", "type") and not (tok(j + 1).is_("ident", "from") or tok(j + 1).is_("punct", ",")):
                j += 1  # `import type X from`, but not `import type from "x"`
            if tok(j).kind == "ident":  # default binding
                if tok(j + 1).is_("punct", "="):
                    continue  # TypeScript `import x = require("y")`: the require is read on its own
                j += 1
                if tok(j).is_("punct", ","):
                    j += 1
            star = tok(j).is_("punct", "*")
        else:
            if tok(j).is_("ident", "type"):
                j += 1
            star = tok(j).is_("punct", "*")
            if not (star or tok(j).is_("punct", "{")):
                continue  # export const, export default, export =, ...
        if star:
            j += 1
            if tok(j).is_("ident", "as"):
                j = binding_name(j + 1)
        elif tok(j).is_("punct", "{"):
            j = _group_end(toks, j)
        if tok(j).is_("ident", "from") and tok(j + 1).kind == "str":
            out.append((t.start, "literal", tok(j + 1).text))
        elif t.text == "import" or star:
            out.append((t.start, "unsupported", None))
    return out


def js_package(spec: str) -> str:
    parts = spec.split("/")
    return "/".join(parts[:2]) if spec.startswith("@") else parts[0]


def check_js_file(rel: str, src: str, cfg: Config, root: Path | None = None) -> list[Violation]:
    lines = Lines(src)
    try:
        toks = lex_js(src)
    except LexError as e:
        return [Violation(PARSE_RULE, rel, lines.line(e.offset), f"cannot tokenize: {e.message}")]
    found: list[Violation] = []
    for offset, kind, spec in js_imports(toks):
        line = lines.line(offset)
        if kind == "unsupported":
            found.append(Violation(PARSE_RULE, rel, line, "unsupported import/export declaration syntax"))
            continue
        if spec is None:
            for rule in cfg.of("js_literal_imports"):
                if rule.applies(rel):
                    found.append(Violation(rule.id, rel, line, "import or require with a non-literal specifier"))
            continue
        if spec.startswith("."):
            for target in reach_targets(root, normalize(str(PurePosixPath(rel).parent / spec))):
                for rule in cfg.of("path_ban"):
                    if Globs(rule.data["to"]).reaches(target):
                        found.append(Violation(rule.id, rel, line, f"imports non-production path {target}"))
            continue
        pkg = js_package(spec)
        for rule in cfg.of("js_import"):
            if rule.applies(rel) and Globs(rule.data["packages"]).match(pkg):
                found.append(Violation(rule.id, rel, line, f"imports {spec}"))
    return found


# --------------------------------------------------------------------------- Cargo

@dataclass
class Package:
    name: str
    dir: str  # checkout-relative; "" for a root package
    rel_manifest: str
    data: dict
    bindings: dict[str, str] = field(default_factory=dict)  # import root -> crate, where renamed


def _dep_entries(data: dict, sections: tuple[str, ...]):
    tables = [data] + [t for t in data.get("target", {}).values() if isinstance(t, dict)]
    for t in tables:
        for section in sections:
            for key, spec in t.get(section, {}).items():
                yield key, spec


def _resolve(key: str, spec, manifest_dir: Path, root: Path, ws_deps: dict) -> tuple[str, Path | None]:
    """(upstream package name, path-dependency directory or None)."""
    base = manifest_dir
    if isinstance(spec, dict) and spec.get("workspace") is True:
        inherited = ws_deps.get(key, {})
        spec = inherited if isinstance(inherited, dict) else {}
        base = root
    if not isinstance(spec, dict):
        return key, None
    path = spec.get("path")
    return spec.get("package", key), (base / path if isinstance(path, str) else None)


def _rel(root: Path, path: Path) -> str | None:
    return normalize(os.path.relpath(path, root).replace(os.sep, "/"))


def _read_toml(path: Path, rel: str, found: list[Violation]) -> dict | None:
    try:
        return tomllib.loads(path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        found.append(Violation(PARSE_RULE, rel, 1, f"cannot read manifest: {e}"))
        return None


def workspace_packages(root: Path, found: list[Violation]) -> tuple[list[Package], dict]:
    """Every workspace package, as Cargo finds them: the root package, `members` globs
    minus `exclude`, and path dependencies inside the checkout, recursively."""
    root_manifest = root / "Cargo.toml"
    if not root_manifest.exists():
        return [], {}
    root_data = _read_toml(root_manifest, "Cargo.toml", found)
    if root_data is None:
        return [], {}
    ws = root_data.get("workspace", {})
    ws_deps = ws.get("dependencies", {})
    exclude = [e for e in (normalize(x) for x in ws.get("exclude", [])) if e]
    queue: list[str] = []

    def add(directory: Path) -> None:
        rel = _rel(root, directory)
        if rel is None or rel in queue or not (directory / "Cargo.toml").is_file():
            return
        if any(rel == e or rel.startswith(e + "/") for e in exclude):
            return
        queue.append(rel)

    if "package" in root_data:
        queue.append("")
    for member in ws.get("members", []):
        for d in sorted(root.glob(member)):
            if d.is_dir():
                add(d)
    packages, k = [], 0
    while k < len(queue):
        rel = queue[k]
        k += 1
        manifest = root / rel / "Cargo.toml" if rel else root_manifest
        rel_manifest = f"{rel}/Cargo.toml" if rel else "Cargo.toml"
        data = root_data if not rel else _read_toml(manifest, rel_manifest, found)
        if data is None or "package" not in data or (rel and "workspace" in data):
            continue
        pkg = Package(data["package"].get("name", manifest.parent.name), rel, rel_manifest, data)
        for key, spec in _dep_entries(data, PROD_DEP_SECTIONS + DEV_DEP_SECTIONS):
            real, path = _resolve(key, spec, manifest.parent, root, ws_deps)
            # The import root is the Cargo key when it renames the package; otherwise
            # the package's library target name, which an in-checkout dependency's
            # manifest can set with [lib] name. (A registry crate's is assumed default.)
            import_root = key.replace("-", "_")
            if path is not None and real == key and (path / "Cargo.toml").is_file():
                try:
                    lib = tomllib.loads((path / "Cargo.toml").read_text()).get("lib", {}).get("name")
                except (OSError, tomllib.TOMLDecodeError):
                    lib = None
                if isinstance(lib, str):
                    import_root = lib.replace("-", "_")
            if import_root != real.replace("-", "_"):
                pkg.bindings[import_root] = real.replace("-", "_")
            if path is not None:
                add(path)
        packages.append(pkg)
    return packages, ws_deps


def check_cargo(root: Path, cfg: Config, packages: list[Package], ws_deps: dict) -> list[Violation]:
    found: list[Violation] = []
    for pkg in packages:
        if cfg.non_production.match(pkg.rel_manifest):
            continue
        manifest = root / pkg.rel_manifest
        lines = manifest.read_text().splitlines()
        for key, spec in _dep_entries(pkg.data, PROD_DEP_SECTIONS):
            real, path = _resolve(key, spec, manifest.parent, root, ws_deps)
            line = next((k + 1 for k, l in enumerate(lines)
                         if re.match(rf"\s*(\[[^\]]*[.\s])?{re.escape(key)}(?![\w-])", l)), 1)
            for rule in cfg.of("crate_ban"):
                if real in rule.data["crates"]:
                    found.append(Violation(rule.id, pkg.rel_manifest, line, f"{pkg.name} depends on {real}"))
            for rule in cfg.of("crate_scope"):
                if real in rule.data["crates"] and pkg.name not in rule.data["packages"]:
                    found.append(Violation(rule.id, pkg.rel_manifest, line,
                                           f"{pkg.name} depends on {real}; only {', '.join(rule.data['packages'])} may"))
            if path is not None:
                for target in reach_targets(root, _rel(root, path)):
                    for rule in cfg.of("path_ban"):
                        if Globs(rule.data["to"]).reaches(target):
                            found.append(Violation(rule.id, pkg.rel_manifest, line, f"{pkg.name} depends on path {target}"))
    return found


def _owner(rel: str, packages: list[Package]) -> Package | None:
    best = None
    for pkg in packages:
        if (pkg.dir == "" or rel.startswith(pkg.dir + "/")) and (best is None or len(pkg.dir) > len(best.dir)):
            best = pkg
    return best


# --------------------------------------------------------------------------- run

def run(root: Path, config: Path) -> list[Violation]:
    cfg = load_config(config)
    found: list[Violation] = []
    packages, ws_deps = workspace_packages(root, found)
    found += check_cargo(root, cfg, packages, ws_deps)

    files, skipped, links = walk(root, cfg.skip)
    protected = [f"{p.dir}/src/" if p.dir else "src/" for p in packages]
    protected += [p for r in cfg.rules for p in r.scope_in.prefixes()]
    for d in skipped:
        hidden = sorted({p for p in protected if (d + "/").startswith(p) or p.startswith(d + "/")})
        if hidden:
            found.append(Violation(SKIP_RULE, d, 1, f"a `skip` glob hides this directory, which holds production source ({', '.join(hidden)})"))

    for rel in links:  # a production path that is really a non-production one
        if cfg.non_production.match(rel):
            continue
        for target in reach_targets(root, rel)[1:]:
            for rule in cfg.of("path_ban"):
                if Globs(rule.data["to"]).reaches(target):
                    found.append(Violation(rule.id, rel, 1, f"symlink to non-production path {target}"))

    for rel, full in files:
        if cfg.non_production.match(rel):
            continue
        ext = PurePosixPath(rel).suffix
        for rule in cfg.of("source_language"):
            if rule.applies(rel) and ext in rule.data["forbid_extensions"]:
                found.append(Violation(rule.id, rel, 1, f"{ext} source file"))
        if ext in RUST_EXT:
            owner = _owner(rel, packages)
            found += check_rust_file(rel, full.read_text(errors="replace"), cfg, owner.bindings if owner else {}, root)
        elif ext in JS_EXT:
            found += check_js_file(rel, full.read_text(errors="replace"), cfg, root)
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
