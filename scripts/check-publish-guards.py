#!/usr/bin/env python3
"""Workflow policy for .github/workflows: static checks, run in ci › lint.

What this is, and what it is not: a static lint over the workflow files of
this revision. It reads their YAML (a strict subset: what it cannot read is
exit 2), matches patterns over each `run:` script and judges what a job may
do. It cannot see a publish hidden in a script a step calls, a workflow on
another branch or tag, or a repository setting. The security boundary lies
elsewhere: the environments' deployment rules (v* tags only) and the tag
ruleset, which the registries' trusted publishers rely on (RELEASING.md §
Publishing). This check keeps this revision's publishing jobs from drifting
away from those rules through an unnoticed edit.

  check-publish-guards.py [--workflows DIR]             the publishing-job policy
  check-publish-guards.py --toolchains [--root DIR]     the one-toolchain policy
  check-publish-guards.py --self-test                   both, on the real tree and on mutated copies

Every workflow (*.yml or *.yaml; any other entry in the directory fails):
read-only top-level permissions; no reference to the `secrets` context
anywhere, in any letter case (index syntax, toJSON, a workflow-level env and
`secrets: inherit` included; GitHub resolves contexts and functions whatever
their case, and no workflow here reads a secret); no job-level `uses:` other
than a local workflow of the same directory, which this check reads.

A job is PRIVILEGED when it publishes (cargo publish/yank/owner, npm
publish/unpublish/deprecate/dist-tag/owner/access/stage, pnpm/yarn/bun
publish, napi pre-publish unless it uploads nothing, pub publish,
twine/uv/maturin/poetry publish, gh release create/upload/edit/delete or a
`gh api` releases call, pypa/gh-action-pypi-publish,
rust-lang/crates-io-auth-action), with options before the subcommand,
npm's abbreviations and backslash-continued lines included, or when it
holds a write permission (`id-token: write` included: it can mint registry
tokens). Every privileged job must:

  guard        run from a release tag only: its raw `if:` has the top-level
               conjunct startsWith(github.ref, 'refs/tags/v'), never under
               `||`; no `#`; no `${{` unless one expression spans the whole
               value (text around one makes a string, always true); and no
               always(), cancelled(), failure() or success(), in any case
  needs        need the job that inspects what it uploads (INSPECTION)
  environment  name one of crates-io · npm · release · pub
  permissions  declare its own, minimal: contents read (write only to run
               `gh release`), id-token write only to publish to a registry
  pinned       use every action by a 40-hex commit SHA (actions/* included)
               and no local `./` action (its steps are not judged here);
               container and service images by @sha256 digest; check out
               with `persist-credentials: false`; no shell other than bash;
               no `${{ }}` interpolated into a script
  gate         run, as its first step after the checkout, exactly
               `python3 scripts/package-inspect.py versions --tag "$GITHUB_REF" --strict --quiet`
               (no key but name and working-directory, nothing joined to it)
  toolchains   Rust through rustup or a pinned action at RUST_TOOLCHAIN,
               resolved step > job > workflow to an exact x.y.z and never set
               in a script, no rust-toolchain file; pnpm, Flutter, Dart and
               every other setup action or installer tool at an exact x.y.z
  node/python  actions/setup-node and actions/setup-python may name a
               release line (node 24, python 3.12) or an exact version, never
               lts/*, latest, current, node, a range or nothing: security
               point releases arrive without a repository change, and the
               action and its version manifest are first-party. A step after
               the setup and before the first publishing step prints the
               resolved versions exactly as PRINTED says (npm >= 11.5.1
               asserted)
  locked       fetch nothing unlocked: npm install/i/add/update/exec/x/init/
               create, pnpm without --frozen-lockfile, npx/pnpx/bunx/dlx,
               yarn without --immutable, corepack, pip or uv pip without
               --require-hashes, pipx, uvx, uv tool, cargo install without
               --locked and an exact --version, cargo binstall, pub get/add/
               upgrade without --enforce-lockfile, pub global, dart run,
               gem/go install, a download piped into a shell
  publish      `cargo publish` with --locked --no-verify; `npm publish` whose
               operand (its first non-option word) is a tarball (`*.tgz`, or a
               variable named *tgz) and which carries --ignore-scripts as an
               option; never pnpm, yarn or bun publish. A trailing shell
               comment is stripped from every code line first, so the required
               words cannot hide in one
  concurrency  join publish-${{ github.workflow }}-${{ github.ref }}, never
               cancelling, so a re-run on the tag waits instead of racing

`napi pre-publish` with both --skip-optional-publish and --no-gh-release (or
--dry-run) uploads nothing (@napi-rs/cli 3.7.3, cli/src/api/pre-publish.ts):
it only writes optionalDependencies, so it does not make a job privileged.

ONE NAMED EXCEPTION, keyed to three exact steps of flutter.yml › publish:
the setup-dart and flutter-action steps (stable-channel SDKs) and its
`flutter pub get` (no lockfile). The job is dormant (pub.dev's first upload
is manual and the package's crate still depends on a path outside it), and
pub resolution runs no package code. Another step with the same finding is
not waived; a waiver that matches nothing fails, so it is removed with the
fix (RELEASING.md § Publishing).

The one-toolchain policy (--toolchains), over every workflow: every
RUST_TOOLCHAIN assignment (workflow, job or step env) holds the same exact
x.y.z; no script assigns RUST_TOOLCHAIN, FUZZ_TOOLCHAIN or RUSTUP_TOOLCHAIN,
and no env sets RUSTUP_TOOLCHAIN; FUZZ_TOOLCHAIN is a dated
nightly-YYYY-MM-DD; every toolchain a leg names (an action's toolchain or
rust-toolchain input, flow mappings included, rustup and `cargo +`
arguments, --toolchain) is one of those variables; the one literal is the
MSRV leg, dtolnay/rust-toolchain at the rust-version the root manifest
declares (pinned by SHA, it names that version as its toolchain input); no
step names a rust-toolchain file, and none exists in the tree (it outranks
`rustup default` in its directory and below).

The YAML reader covers the subset the workflows here use (block mappings
and sequences, flow collections, quoted and plain scalars, literal and
folded block scalars, comments ending where YAML ends them) and refuses
anything else: anchors, aliases, tags, multi-line plain scalars, a comment
inside a flow collection. Exit 0 compliant · 1 a violation · 2 cannot judge
(an unreadable workflow). Stdlib only, Python >= 3.11.
"""

import argparse
import contextlib
import dataclasses
import io
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent

ENVIRONMENTS = {"crates-io", "npm", "release", "pub"}
GUARD = "startsWith(github.ref, 'refs/tags/v')"
CONCURRENCY = "publish-${{ github.workflow }}-${{ github.ref }}"
GATE_RUN = 'python3 scripts/package-inspect.py versions --tag "$GITHUB_REF" --strict --quiet'
SHA = re.compile(r"[0-9a-f]{40}")
EXACT = re.compile(r"\d+\.\d+\.\d+")
DIGEST = re.compile(r"@sha256:[0-9a-f]{64}$")

# The job each publishing job must need: the one that inspects (for the iOS
# zip, builds) what it uploads. A publishing job missing here fails.
INSPECTION = {
    ("crates-publish.yml", "publish"): "package",
    ("npm-publish.yml", "publish-native"): "pack-native",
    ("npm-publish.yml", "publish-wasm"): "build-wasm",
    ("python.yml", "release"): "inspect",
    ("mobile.yml", "ios-release"): "ios",
    ("flutter.yml", "publish"): "test",
}

# What a job relying on the setup-node/-python relaxation runs, verbatim, in
# one step after the setup and before its first publishing step.
PRINTED = {
    "actions/setup-node": "\n".join((
        "node --version",
        "v=$(npm --version)",
        'echo "npm $v"',
        "if [ \"$(printf '%s\\n' 11.5.1 \"$v\" | sort -V | head -n1)\" != 11.5.1 ]; then",
        '  echo "::error::npm $v cannot publish through trusted publishing (needs >= 11.5.1)"',
        "  exit 1",
        "fi",
    )),
    "actions/setup-python": "python --version\npip --version",
}

DORMANT = "dormant until pub.dev's manual first upload and the fix of the path dependency"
# (workflow, job) -> the exact steps waived, each with the one finding it may
# carry; nothing else is ever waived.
EXCEPTIONS = {
    ("flutter.yml", "publish"): (
        ("floating-dart-sdk", {"uses": "dart-lang/setup-dart@6afc89df92d6eb3834022f73cd65adc8cdfcb92d"}, DORMANT),
        ("floating-flutter-sdk", {"uses": "subosito/flutter-action@e938fdf56512cc96ef2f93601a5a40bde3801046",
                                  "with": {"channel": "stable"}}, DORMANT),
        ("unlocked-pub-get", {"run": "flutter pub get"}, f"{DORMANT}; pub resolution runs no package code"),
    ),
}

# A step publishes when it runs a tool with one of these subcommands as a later
# word of the same command line, matches PUBLISHING_RUN, or uses one of
# PUBLISHING_ACTIONS. npm accepts any unique prefix of a command.
NPM_UPLOAD = frozenset({"pu", "pub", "publ", "publi", "publish"})
PUBLISHING_TOOLS = (
    ("cargo", frozenset({"publish", "yank", "owner"}), "cargo publish"),
    ("npm", NPM_UPLOAD | {"unpublish", "deprecate", "dist-tag", "dist-tags", "owner", "access", "stage"},
     "npm publish"),
    ("pnpm", frozenset({"publish"}), "pnpm publish"),
    ("yarn", frozenset({"publish"}), "yarn publish"),
    ("bun", frozenset({"publish"}), "bun publish"),
    ("twine", frozenset({"upload"}), "twine upload"),
    ("uv", frozenset({"publish"}), "uv publish"),
    ("maturin", frozenset({"publish", "upload"}), "maturin publish"),
    ("poetry", frozenset({"publish"}), "poetry publish"),
)
PUBLISHING_RUN = (
    (re.compile(r"\bnapi\s+pre-?publish\b"), "napi pre-publish"),
    (re.compile(r"\bpub\s+(publish|lish)\b"), "pub publish"),
    (re.compile(r"\bgh\s+release\s+(create|upload|edit|delete)\b"), "gh release"),
    (re.compile(r"\bgh\s+api\b.*\breleases\b"), "gh release"),
)
PUBLISHING_ACTIONS = ("pypa/gh-action-pypi-publish", "rust-lang/crates-io-auth-action")
NPM_FETCH = frozenset({"install", "i", "in", "ins", "inst", "insta", "instal", "isnt", "isnta", "isntal",
                       "isntall", "add", "update", "up", "upgrade", "udpate", "exec", "x", "init", "create",
                       "innit", "install-test", "it"})
TGZ_ARG = re.compile(r"\S*\.tgz|\$\{?\w*tgz\}?")
# npm publish options that take the next word as their value: that word is not
# the package operand (`npm publish --access public <tgz>`).
NPM_VALUE_OPTS = frozenset({"--access", "--tag", "--otp", "--registry", "-w", "--workspace", "--userconfig",
                            "--//registry.npmjs.org/:_authToken"})

# Toolchain names a leg may use instead of a literal: the variables.
NAMED_INPUTS = {"${{env.RUST_TOOLCHAIN}}", "${{env.FUZZ_TOOLCHAIN}}", "${{inputs.toolchain||env.RUST_TOOLCHAIN}}"}
NAMED_VARIABLE = re.compile(r"\$\{?(RUST_TOOLCHAIN|FUZZ_TOOLCHAIN)\}?")
# Actions with a rule of their own; any other setup-* / *-setup action needs an exact version.
SPECIFIC_SETUP = {"dtolnay/rust-toolchain", "pnpm/action-setup", "subosito/flutter-action", "dart-lang/setup-dart",
                  "actions/setup-node", "actions/setup-python"}


# ---------------------------------------------------------------- YAML subset


class YamlError(ValueError):
    pass


_KEY = re.compile(r"""([A-Za-z0-9_][A-Za-z0-9_.-]*|"[^"]*"|'[^']*')[ \t]*:(?:[ \t]+|$)""")
_ESCAPES = {"n": "\n", "t": "\t", "r": "\r", '"': '"', "\\": "\\", "/": "/", "0": "\0", " ": " "}


def _is_dash(content: str) -> bool:
    return content == "-" or content.startswith("- ")


def _plain_comment(text: str) -> str:
    """A plain scalar without its comment. YAML starts a comment at a `#` that
    begins the text or follows whitespace; quotes inside a plain scalar are
    ordinary characters, so they never hide one."""
    for i, ch in enumerate(text):
        if ch == "#" and (i == 0 or text[i - 1] in " \t"):
            return text[:i].rstrip()
    return text.rstrip()


def _double_quoted(text: str, start: int) -> tuple:
    out, i = [], start + 1
    while i < len(text):
        ch = text[i]
        if ch == '"':
            return "".join(out), i + 1
        if ch == "\\":
            nxt = text[i + 1:i + 2]
            if nxt == "u":
                out.append(chr(int(text[i + 2:i + 6], 16)))
                i += 6
                continue
            if nxt not in _ESCAPES:
                raise YamlError(f"unsupported escape \\{nxt}")
            out.append(_ESCAPES[nxt])
            i += 2
            continue
        out.append(ch)
        i += 1
    raise YamlError("unterminated double-quoted scalar")


def _single_quoted(text: str, start: int) -> tuple:
    out, i = [], start + 1
    while i < len(text):
        if text[i] == "'":
            if text[i + 1:i + 2] == "'":
                out.append("'")
                i += 2
                continue
            return "".join(out), i + 1
        out.append(text[i])
        i += 1
    raise YamlError("unterminated single-quoted scalar")


def _flow(text: str, i: int) -> tuple:
    """A flow collection or scalar starting at text[i] → (value, next index)."""
    while text[i] == " ":
        i += 1
    if text[i] in "[{":
        closing, items, mapping = ("]" if text[i] == "[" else "}"), [], {}
        i += 1
        while True:
            while text[i] == " ":
                i += 1
            if text[i] == closing:
                return (items if closing == "]" else mapping), i + 1
            if closing == "}":
                key, i = _flow(text, i)
                while text[i] == " ":
                    i += 1
                if text[i] != ":":
                    raise YamlError(f"flow mapping entry without ':' in {text!r}")
                mapping[key], i = _flow(text, i + 1)
            else:
                value, i = _flow(text, i)
                items.append(value)
            while text[i] == " ":
                i += 1
            if text[i] == ",":
                i += 1
            elif text[i] != closing:
                raise YamlError(f"unexpected {text[i]!r} in {text!r}")
    if text[i] == '"':
        return _double_quoted(text, i)
    if text[i] == "'":
        return _single_quoted(text, i)
    end = i
    while end < len(text) and text[end] not in ",]}" and not (text[end] == ":" and text[end + 1:end + 2] == " "):
        if text[end] == "#" and text[end - 1] in " \t":
            raise YamlError(f"a comment inside a flow collection: {text!r}")
        end += 1
    return text[i:end].strip(), end


class _Reader:
    def __init__(self, text: str):
        self.lines = text.split("\n")
        if any("\t" in line[:len(line) - len(line.lstrip())] for line in self.lines):
            raise YamlError("tab indentation")
        self.i = 0

    def peek(self):
        """(indent, content) of the next significant line, or None."""
        while self.i < len(self.lines):
            line = self.lines[self.i]
            stripped = line.strip()
            if stripped and not stripped.startswith("#"):
                if stripped in ("---", "...") or stripped.startswith("%"):
                    raise YamlError(f"line {self.i + 1}: documents and directives are not supported")
                return len(line) - len(line.lstrip(" ")), line.lstrip(" ")
            self.i += 1
        return None

    def node(self, indent: int):
        head = self.peek()
        if head is None or head[0] < indent:
            return None
        return self.sequence(head[0]) if _is_dash(head[1]) else self.mapping(head[0])

    def mapping(self, indent: int) -> dict:
        result: dict = {}
        while (head := self.peek()) is not None and head[0] == indent and not _is_dash(head[1]):
            match = _KEY.match(head[1])
            if not match:
                raise YamlError(f"line {self.i + 1}: expected `key:`, got {head[1]!r}")
            key = match.group(1)
            key = key[1:-1] if key[:1] in "'\"" else key
            if key in result:
                raise YamlError(f"line {self.i + 1}: duplicate key {key!r}")
            self.i += 1
            result[key] = self.value(head[1][match.end():], indent)
        if head is not None and head[0] > indent:
            raise YamlError(f"line {self.i + 1}: unexpected indentation")
        return result

    def sequence(self, indent: int) -> list:
        items = []
        while (head := self.peek()) is not None and head[0] == indent and _is_dash(head[1]):
            rest = head[1][1:]
            content = rest.lstrip(" ")
            column = indent + 1 + len(rest) - len(content)
            if content and content[0] not in "[{\"'" and _KEY.match(content):
                # `- key: value`: a mapping whose first line shares the dash
                self.lines[self.i] = " " * column + content
                items.append(self.mapping(column))
            else:
                self.i += 1
                items.append(self.value(content, indent))
        return items

    def value(self, text: str, indent: int):
        text = text.strip()
        if text[:1] in ("|", ">"):
            return self.block_scalar(text, indent)
        if text[:1] in ("&", "*", "!"):
            raise YamlError(f"line {self.i}: anchors, aliases and tags are not supported")
        if text[:1] in ("[", "{", '"', "'"):
            # A quoted scalar or a flow collection: a comment may only follow it.
            try:
                value, end = _flow(text, 0)
            except IndexError:
                raise YamlError(f"line {self.i}: unterminated {text!r}") from None
            if text[end:].strip() and not re.match(r"[ \t]+#", text[end:]):
                raise YamlError(f"line {self.i}: trailing text after {text[:end]!r}")
            return value
        text = _plain_comment(text)
        if not text:
            head = self.peek()
            if head is None:
                return None
            if head[0] > indent:
                return self.node(head[0])
            if head[0] == indent and _is_dash(head[1]):
                return self.sequence(indent)
            return None
        head = self.peek()
        if head is not None and head[0] > indent and not _is_dash(head[1]):
            raise YamlError(f"line {self.i + 1}: multi-line plain scalars are not supported")
        return text

    def block_scalar(self, header: str, indent: int) -> str:
        match = re.fullmatch(r"([|>])([+-]?)\s*(#.*)?", header)
        if not match:
            raise YamlError(f"line {self.i}: unsupported block scalar header {header!r}")
        style, chomp = match.group(1), match.group(2)
        raw = []
        while self.i < len(self.lines):
            line = self.lines[self.i]
            if line.strip() and len(line) - len(line.lstrip(" ")) <= indent:
                break
            raw.append(line)
            self.i += 1
        body = [line for line in raw if line.strip()]
        if not body:
            return ""
        block = len(body[0]) - len(body[0].lstrip(" "))
        if any(len(line) - len(line.lstrip(" ")) < block for line in body):
            raise YamlError(f"line {self.i}: a block scalar line is less indented than its first line")
        lines = [line[block:] if line.strip() else "" for line in raw]
        while lines and lines[-1] == "":
            lines.pop()
        trailing = len(raw) - len(lines)
        if style == "|":
            text = "\n".join(lines)
        else:  # folded: a break between two normal lines becomes a space
            text, normal = "", False
            for line in lines:
                if line == "":
                    text += "\n"
                    normal = False
                    continue
                more = line.startswith(" ")
                if text and not text.endswith("\n"):
                    text += " " if normal and not more else "\n"
                text += line
                normal = not more
        if chomp == "-":
            return text
        if chomp == "+":
            return text + "\n" * (1 + trailing)
        return text + "\n"


def load_yaml(text: str):
    reader = _Reader(text)
    value = reader.node(0)
    if reader.peek() is not None:
        raise YamlError(f"line {reader.i + 1}: unparsed content")
    return value


# ---------------------------------------------------------------- shared helpers


@dataclasses.dataclass
class Finding:
    workflow: str
    job: str
    code: str
    message: str
    step: int = -1  # index in the job's steps; -1 for a job- or workflow-level finding
    waived: str = ""


def _text(node) -> str:
    """Every key and string inside a node, newline-joined."""
    if isinstance(node, dict):
        return "\n".join(f"{k}\n{_text(v)}" for k, v in node.items())
    if isinstance(node, list):
        return "\n".join(_text(v) for v in node)
    return "" if node is None else str(node)


def _strip_comment(line: str) -> str:
    """LINE without its trailing shell comment. bash starts a comment at a `#`
    that begins a word — the start of the line or after whitespace — and treats
    one inside '…' or "…" as a literal; a `#` that follows a non-space (a
    parameter expansion like ${#x}, a fragment like a#b) is not a comment."""
    quote = None
    for i, ch in enumerate(line):
        if quote:
            if ch == quote:
                quote = None
        elif ch in "'\"":
            quote = ch
        elif ch == "#" and (i == 0 or line[i - 1] in " \t"):
            return line[:i].rstrip()
    return line


def _code_lines(script) -> list:
    """The command lines of a run script: each line's trailing shell comment
    removed first (a `#` ends the line for bash, continuation backslash
    included), backslash continuations then joined, blank lines dropped."""
    lines, pending = [], ""
    for raw in str(script or "").splitlines():
        line = _strip_comment(raw.strip())
        if not pending and not line:
            continue
        if line.endswith("\\"):
            pending += line[:-1] + " "
            continue
        lines.append((pending + line).strip())
        pending = ""
    if pending.strip():
        lines.append(pending.strip())
    return lines


def _npm_publish_operand(line: str):
    """(operand, option words) of a line running npm's publish subcommand, or
    None. The operand is the first word after the subcommand that is neither an
    option nor an option's value — npm's package spec, what it actually uploads."""
    raw = line.split()
    for i, token in enumerate(raw):
        if re.search(r"(?:^|[=(`'\"/])npm$", token.rstrip("\"'`;)")):
            rest = _words(" ".join(raw[i + 1:]))
            for j, word in enumerate(rest):
                if word in NPM_UPLOAD:
                    after = rest[j + 1:]
                    opts = [w for w in after if w.startswith("-")]
                    k = 0
                    while k < len(after):
                        if after[k].startswith("-"):
                            k += 2 if after[k] in NPM_VALUE_OPTS else 1
                        else:
                            return after[k], opts
                    return None, opts
    return None


def _words(text: str) -> list:
    """Shell words of TEXT, each stripped of the quotes and the punctuation a
    command substitution or a sequence wraps around it."""
    return [word.strip("\"'`;)(") for word in text.split()]


def _invokes(line: str, tool: str, subcommands) -> bool:
    """LINE runs TOOL (a word ending in it: a path, a quote or a command
    substitution may precede it) with one of SUBCOMMANDS as a later word."""
    raw = line.split()
    for i, token in enumerate(raw):
        if re.search(rf"(?:^|[=(`'\"/]){re.escape(tool)}$", token.rstrip("\"'`;)")) and \
                any(word in subcommands for word in _words(" ".join(raw[i + 1:]))):
            return True
    return False


def _uses(step) -> tuple:
    """(action without its ref, ref) of a `uses:` step, or ('', '')."""
    uses = step.get("uses") if isinstance(step, dict) else None
    if not uses:
        return "", ""
    action, _, ref = str(uses).partition("@")
    return action, ref


def _steps(job) -> list:
    return [step if isinstance(step, dict) else {} for step in (job.get("steps") or [])]


def _resolve(key: str, *envs):
    """KEY as the first of ENVS that sets it sees it (step > job > workflow), or None."""
    for env in envs:
        if isinstance(env, dict) and key in env:
            return env[key]
    return None


def workflow_files(workflows: pathlib.Path) -> tuple:
    """(the *.yml and *.yaml files, every other entry of the directory)."""
    entries = sorted(workflows.iterdir())
    files = [path for path in entries if path.is_file() and path.suffix in (".yml", ".yaml")]
    return files, [path.name for path in entries if path not in files]


def load_workflows(workflows: pathlib.Path) -> list:
    """[(file name, parsed workflow)]; a YamlError names the file."""
    loaded = []
    for path in workflow_files(workflows)[0]:
        try:
            wf = load_yaml(path.read_text())
        except YamlError as err:
            raise YamlError(f"{path.name}: {err}") from err
        if not isinstance(wf, dict) or not isinstance(wf.get("jobs"), dict):
            raise YamlError(f"{path.name}: no jobs map")
        loaded.append((path.name, wf))
    return loaded


# ---------------------------------------------------------------- publishing policy


def _top_level(expr: str, op: str) -> list:
    """EXPR split on OP outside parentheses and quotes."""
    parts, depth, quoted, start, i = [], 0, False, 0, 0
    while i < len(expr):
        ch = expr[i]
        if quoted:
            if ch == "'":
                if expr[i + 1:i + 2] == "'":
                    i += 2
                    continue
                quoted = False
        elif ch == "'":
            quoted = True
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        elif depth == 0 and expr.startswith(op, i):
            parts.append(expr[start:i])
            i += len(op)
            start = i
            continue
        i += 1
    parts.append(expr[start:])
    return [part.strip() for part in parts]


def _unwrap(expr: str) -> str:
    """An expression without outer parentheses or extra whitespace."""
    expr = expr.strip()
    while expr.startswith("(") and expr.endswith(")"):
        depth = 0
        for i, ch in enumerate(expr):
            depth += (ch == "(") - (ch == ")")
            if depth == 0 and i < len(expr) - 1:
                break
        else:
            expr = expr[1:-1].strip()
            continue
        break
    return re.sub(r"\s+", " ", expr)


def guarded(condition) -> str:
    """'' when the raw `if:` value CONDITION runs on a release tag only, else why not."""
    if condition is None:
        return "no `if:`: the job runs on every ref"
    raw = str(condition)
    if "#" in raw:
        return f"`if: {raw!r}` holds a `#`: where the value ends depends on who reads it"
    expr = raw
    if "${{" in raw:
        whole = re.fullmatch(r"\$\{\{(.*)\}\}", raw, re.DOTALL)
        if not whole or "${{" in whole.group(1) or "}}" in whole.group(1):
            return f"`if: {raw!r}`: text around `${{{{ }}}}` makes a string, which is always true"
        expr = whole.group(1)
    # GitHub's expression functions ignore case: ALWAYS() is always().
    if re.search(r"\b(always|cancelled|failure|success)\s*\(", expr, re.IGNORECASE):
        return f"`if: {raw!r}` calls a status function: the job would run whatever its needs did"
    expr = _unwrap(expr)
    if len(_top_level(expr, "||")) > 1:
        return f"`if: {raw!r}` puts the tag guard under ||"
    if GUARD not in (_unwrap(part) for part in _top_level(expr, "&&")):
        return f"`if: {raw!r}` has no top-level {GUARD}"
    return ""


def publishing_steps(steps) -> list:
    """(index, what) of every step that publishes."""
    found = []
    for index, step in enumerate(steps):
        action, _ = _uses(step)
        if any(action == a or action.startswith(a + "/") for a in PUBLISHING_ACTIONS):
            found.append((index, action))
            continue
        for line in _code_lines(step.get("run")):
            found += [(index, what) for tool, subcommands, what in PUBLISHING_TOOLS
                      if _invokes(line, tool, subcommands)]
            for pattern, what in PUBLISHING_RUN:
                if not pattern.search(line):
                    continue
                if what == "napi pre-publish" and ("--dry-run" in line or (
                        "--skip-optional-publish" in line and
                        re.search(r"--no-gh-release\b|--gh-release[= ]false\b", line))):
                    continue  # writes optionalDependencies, uploads nothing
                found.append((index, what))
    return found


def _write_permissions(perms) -> list:
    if isinstance(perms, str):
        return ["write-all"] if perms.strip() == "write-all" else []
    if isinstance(perms, dict):
        return [scope for scope, level in perms.items() if str(level) == "write"]
    return []


def _unlocked(line: str):
    """(code, message) when a run line fetches or runs an unlocked dependency, else None."""
    if re.search(r"\b(flutter|dart)\s+pub\s+(get|upgrade|add|downgrade)\b", line) and \
            "--enforce-lockfile" not in line:
        return "unlocked-pub-get", f"`{line}` resolves without --enforce-lockfile"
    if re.search(r"\b(flutter|dart)\s+pub\s+(global|run)\b|\bdart\s+run\b", line):
        return "unlocked-install", f"`{line}` runs package code fetched on demand"
    if _invokes(line, "npm", NPM_FETCH):
        return "unlocked-install", f"`{line}`: npm fetches without a lockfile (`npm ci` is the locked form)"
    if _invokes(line, "pnpm", {"install", "i", "add", "update", "up"}) and \
            not re.search(r"--frozen-lockfile(\s|$)", line):
        return "unlocked-install", f"`{line}`: pnpm without --frozen-lockfile"
    if re.search(r"\b(npx|pnpx|bunx)\b", line) or _invokes(line, "pnpm", {"dlx"}) or _invokes(line, "yarn", {"dlx"}):
        return "unlocked-install", f"`{line}` fetches a package on demand"
    if re.search(r"\byarn\b", line) and not re.search(r"--(immutable|frozen-lockfile)\b", line):
        return "unlocked-install", f"`{line}`: yarn without --immutable"
    if re.search(r"\bcorepack\b", line):
        return "unlocked-install", f"`{line}`: corepack fetches package managers on demand"
    if re.search(r"\bpip3?\s+install\b|\bpython[\d.]*\s+-m\s+pip\s+install\b|\buv\s+pip\s+install\b", line) and \
            "--require-hashes" not in line or re.search(r"\bpipx\b", line):
        return "unlocked-install", f"`{line}`: pip without --require-hashes"
    if re.search(r"\buvx\b", line) or _invokes(line, "uv", {"tool"}):
        return "unlocked-install", f"`{line}` fetches a tool on demand"
    if _invokes(line, "cargo", {"binstall"}):
        return "unlocked-install", f"`{line}`: cargo binstall downloads a prebuilt binary"
    if _invokes(line, "cargo", {"install"}) and not (
            "--locked" in _words(line) and re.search(r"--version[= ]=?\d+\.\d+\.\d+\b", line)):
        return "unlocked-install", f"`{line}`: cargo install needs --locked and an exact --version"
    if re.search(r"\b(gem|go)\s+(install|get)\b", line):
        return "unlocked-install", f"`{line}`"
    if re.search(r"\b(curl|wget)\b.*\|\s*(sudo\s+)?((ba|z|da)?sh|python3?|node)\b", line):
        return "unlocked-install", f"`{line}` pipes a download into an interpreter"
    return None


def _toolchain_findings(step) -> list:
    """Floating-tool findings of one step of a privileged job."""
    found = []
    action, ref = _uses(step)
    inputs = step.get("with") if isinstance(step.get("with"), dict) else {}
    if action == "dtolnay/rust-toolchain" and \
            re.sub(r"\s+", "", str(inputs.get("toolchain", ref))) != "${{env.RUST_TOOLCHAIN}}":
        found.append(("floating-rust", f"{action} toolchain {inputs.get('toolchain', ref)!r}, not RUST_TOOLCHAIN"))
    if action == "pnpm/action-setup" and not EXACT.fullmatch(str(inputs.get("version", ""))):
        found.append(("floating-pnpm", f"pnpm version {inputs.get('version')!r} is not an exact x.y.z"))
    if action == "subosito/flutter-action" and not EXACT.fullmatch(str(inputs.get("flutter-version", ""))):
        found.append(("floating-flutter-sdk",
                      f"Flutter SDK from channel {inputs.get('channel', 'stable')!r}, no exact flutter-version"))
    if action == "dart-lang/setup-dart" and not EXACT.fullmatch(str(inputs.get("sdk", ""))):
        found.append(("floating-dart-sdk", f"Dart SDK {inputs.get('sdk', 'stable')!r}, not an exact version"))
    if action in ("actions/setup-node", "actions/setup-python"):
        node = action.endswith("node")
        key = "node-version" if node else "python-version"
        version = str(inputs.get(key, "")).strip()
        if not SHA.fullmatch(ref):
            found.append(("floating-runtime", f"{action}@{ref}: a version line needs the action pinned by SHA"))
        if not re.fullmatch(r"\d+" if node else r"\d+\.\d+", version) and not EXACT.fullmatch(version):
            found.append(("floating-runtime", f"{action} {key} {version!r}: a release line or an exact version"))
    name = action.rsplit("/", 1)[-1]
    if action and action not in SPECIFIC_SETUP and (name.startswith("setup-") or name.endswith("-setup")):
        versions = {k: str(v).strip() for k, v in inputs.items() if k.endswith("version") or k in ("sdk", "toolchain")}
        if not versions or not all(EXACT.fullmatch(v) for v in versions.values()):
            found.append(("floating-tool", f"{action} {versions or 'with no version input'}: an exact x.y.z"))
    if action == "taiki-e/install-action":
        tools = [tool.strip() for tool in str(inputs.get("tool", "")).split(",") if tool.strip()]
        loose = [tool for tool in tools if not re.fullmatch(r"[A-Za-z0-9_.-]+@\d+\.\d+\.\d+", tool)]
        if not tools or loose:
            found.append(("floating-tool", f"install-action tool {inputs.get('tool')!r}: every tool at @x.y.z"))
    for line in _code_lines(step.get("run")):
        for match in re.finditer(r"\brustup\s+(?:toolchain\s+install|default|override\s+set|run)\s+(\S+)", line):
            if match.group(1).strip("\"'{}$") != "RUST_TOOLCHAIN":
                found.append(("floating-rust", f"`{line}` names {match.group(1)}, not $RUST_TOOLCHAIN"))
        if re.search(r"\b(RUST_TOOLCHAIN|RUSTUP_TOOLCHAIN)=|\bRUSTUP_TOOLCHAIN\b", line):
            found.append(("floating-rust", f"`{line}` sets the toolchain in a script, where no check can resolve it"))
        if re.search(r"rust-toolchain(\.toml)?\b", line):
            found.append(("floating-rust", f"`{line}`: a rust-toolchain file outranks `rustup default`"))
    return found


def _images(job: dict) -> list:
    """(where, image) of the job's container and service images."""
    found = []
    container = job.get("container")
    if container is not None:
        found.append(("container", str(container.get("image") if isinstance(container, dict) else container)))
    services = job.get("services")
    for name, service in (services.items() if isinstance(services, dict) else []):
        found.append((f"service {name}", str(service.get("image") if isinstance(service, dict) else service)))
    return found


def _block(script) -> str:
    return "\n".join(line.rstrip() for line in str(script or "").strip().splitlines())


def check_job(workflow: str, job_id: str, job: dict, wf: dict) -> list:
    steps = _steps(job)
    publishing = publishing_steps(steps)
    if not publishing and not _write_permissions(job.get("permissions")):
        return []
    out: list = []

    def flag(code: str, message: str, step: int = -1) -> None:
        out.append(Finding(workflow, job_id, code, message, step))

    if problem := guarded(job.get("if")):
        flag("guard", problem)
    needs = job.get("needs")
    needs = [needs] if isinstance(needs, str) else [str(n) for n in needs or []]
    inspection = INSPECTION.get((workflow, job_id))
    if inspection is None:
        flag("needs", "a publishing job with no inspection job declared (INSPECTION)")
    elif inspection not in needs or inspection not in wf["jobs"]:
        flag("needs", f"needs {needs}: must include {inspection!r}, the job that inspects what this one uploads")
    env = job.get("environment")
    env_name = env.get("name") if isinstance(env, dict) else env
    if env_name not in ENVIRONMENTS:
        flag("environment", f"environment {env_name!r} is not one of {sorted(ENVIRONMENTS)}")
    perms = job.get("permissions")
    releases = any(what == "gh release" for _, what in publishing)
    registry = any(what != "gh release" for _, what in publishing)
    if not isinstance(perms, dict):
        flag("permissions", f"job-level permissions must be an explicit map, got {perms!r}")
    else:
        for scope, level in perms.items():
            level = str(level)
            if level == "none" or (scope, level) == ("contents", "read") or \
                    ((scope, level) == ("contents", "write") and releases) or \
                    ((scope, level) == ("id-token", "write") and registry):
                continue
            flag("permissions", f"`{scope}: {level}` is more than this job needs")
    for where, image in _images(job):
        if not DIGEST.search(image):
            flag("container", f"{where} image {image!r} is not pinned by @sha256 digest")
    for where, defaults in (("workflow", wf.get("defaults")), ("job", job.get("defaults"))):
        shell = (defaults.get("run") or {}).get("shell") if isinstance(defaults, dict) else None
        if shell is not None and str(shell) != "bash":
            flag("shell", f"{where} defaults run every script through {shell!r}, not bash")
    for level, env_map in (("job", job.get("env")), *((f"step {i + 1}", s.get("env")) for i, s in enumerate(steps))):
        if env_map is not None and not isinstance(env_map, dict):
            flag("floating-rust", f"{level} env {env_map!r} is not a mapping: no check can resolve it")
    for i, step in enumerate(steps):
        uses = step.get("uses")
        action, ref = _uses(step)
        if uses is not None:
            if str(uses).startswith("./"):
                flag("local-action", f"{uses}: a local action's steps are not judged here", i)
            elif str(uses).startswith("docker://"):
                if not DIGEST.search(str(uses)):
                    flag("unpinned-action", f"{uses} is not pinned by digest", i)
            elif not SHA.fullmatch(ref):
                flag("unpinned-action", f"{uses} is not pinned by commit SHA", i)
        if action == "actions/checkout" and str((step.get("with") or {}).get("persist-credentials")) != "false":
            flag("persist-credentials", "actions/checkout without `persist-credentials: false` leaves the job "
                                        "token in .git/config for every later step", i)
        if step.get("shell") is not None and str(step["shell"]) != "bash":
            flag("shell", f"step shell {step['shell']!r}, not bash", i)
        if "${{" in str(step.get("run") or ""):
            flag("expression", "a `${{ }}` interpolated into a script: pass the value through env", i)
        for code, message in _toolchain_findings(step):
            flag(code, message, i)
        rust = _resolve("RUST_TOOLCHAIN", step.get("env"), job.get("env"), wf.get("env"))
        if "RUST_TOOLCHAIN" in _text(step) and not EXACT.fullmatch(str(rust or "")):
            flag("floating-rust", f"RUST_TOOLCHAIN resolves to {rust!r} for this step (step > job > workflow "
                                  "env), not an exact x.y.z", i)
        for line in _code_lines(step.get("run")):
            if hit := _unlocked(line):
                flag(*hit, i)
            if _invokes(line, "cargo", {"publish"}) and not {"--locked", "--no-verify"} <= set(_words(line)):
                flag("cargo-publish", f"`{line}` needs --locked --no-verify (the package job verified this commit)",
                     i)
            if _invokes(line, "npm", NPM_UPLOAD):
                parsed = _npm_publish_operand(line)
                operand, opts = parsed if parsed else (None, [])
                if operand is None or not TGZ_ARG.fullmatch(operand) or \
                        not {"--ignore-scripts", "--ignore-scripts=true"} & set(opts):
                    flag("npm-publish", f"`{line}`: npm publish takes the inspected tarball (`*.tgz`, or a variable "
                                        "named *tgz) as its operand and --ignore-scripts as an option", i)
            for tool in ("pnpm", "yarn", "bun"):
                if _invokes(line, tool, {"publish"}):
                    flag("npm-publish", f"`{line}`: {tool} publish packs a directory and runs its scripts; upload "
                                        "the inspected tarball with npm publish --ignore-scripts", i)
    for level, env_map in (("job", job.get("env")), *((f"step {i + 1}", s.get("env")) for i, s in enumerate(steps))):
        value = env_map.get("RUST_TOOLCHAIN") if isinstance(env_map, dict) else None
        if value is not None and not EXACT.fullmatch(str(value)):
            flag("floating-rust", f"{level} env RUST_TOOLCHAIN {value!r} is not an exact x.y.z")
    checkout = next((i for i, step in enumerate(steps) if _uses(step)[0] == "actions/checkout"), None)
    gate = steps[checkout + 1] if checkout is not None and checkout + 1 < len(steps) else {}
    if set(gate) - {"run", "name", "working-directory"} or str(gate.get("run", "")).strip() != GATE_RUN:
        flag("gate", f"the first step after the checkout must be exactly `{GATE_RUN}`, with no other key")
    first_publish = min((i for i, _ in publishing), default=len(steps))
    for runtime, block in PRINTED.items():
        setups = [i for i, step in enumerate(steps) if _uses(step)[0] == runtime]
        if setups and not any(set(step) <= {"name", "run"} and _block(step.get("run")) == block
                              for step in steps[max(setups) + 1:first_publish]):
            flag("versions-printed", f"{runtime}: print the resolved versions exactly as PRINTED says, in a step "
                                     "after the setup and before the first publishing step")
    concurrency = job.get("concurrency")
    group = concurrency.get("group") if isinstance(concurrency, dict) else concurrency
    cancels = str(concurrency.get("cancel-in-progress", "false")) if isinstance(concurrency, dict) else "false"
    if re.sub(r"\s+", "", str(group)) != re.sub(r"\s+", "", CONCURRENCY) or cancels != "false":
        flag("concurrency", f"concurrency {concurrency!r}: want group {CONCURRENCY}, cancel-in-progress false")
    return out


def _mentions_secrets(node) -> bool:
    """NODE names the secrets context, in any letter case (GitHub resolves
    `${{ SECRETS.X }}` and `toJSON(Secrets)` like their lower-case forms)."""
    if isinstance(node, dict):
        return any(str(key).lower() == "secrets" or _mentions_secrets(key) or _mentions_secrets(value)
                   for key, value in node.items())
    if isinstance(node, list):
        return any(_mentions_secrets(value) for value in node)
    return node is not None and bool(re.search(r"\bsecrets\b", str(node), re.IGNORECASE))


def check(workflows: pathlib.Path) -> tuple:
    """(findings, privileged jobs judged, stale waivers); a YamlError propagates."""
    findings, judged, stale, visited = [], [], [], set()
    findings += [Finding(name, "(directory)", "workflow-file", "not a *.yml or *.yaml workflow: GitHub reads both, "
                         "and nothing else belongs here") for name in workflow_files(workflows)[1]]
    for name, wf in load_workflows(workflows):
        top = wf.get("permissions")
        if not (isinstance(top, dict) and not _write_permissions(top)) and top != "read-all":
            findings.append(Finding(name, "(workflow)", "permissions",
                                    f"top-level permissions must be read-only, got {top!r}"))
        if _mentions_secrets({key: value for key, value in wf.items() if key != "jobs"}):
            findings.append(Finding(name, "(workflow)", "secrets", "references the secrets context"))
        for job_id, job in wf["jobs"].items():
            job = job if isinstance(job, dict) else {}
            if _mentions_secrets(job):
                findings.append(Finding(name, job_id, "secrets", "references the secrets context"))
            uses = job.get("uses")
            local = re.fullmatch(r"\./\.github/workflows/([^/@]+\.ya?ml)", str(uses))
            if uses is not None and not (local and (workflows / local.group(1)).is_file()):
                findings.append(Finding(name, job_id, "reusable-workflow",
                                        f"`uses: {uses}`: only a local workflow of this directory, which is read"))
            if publishing_steps(_steps(job)) or _write_permissions(job.get("permissions")):
                judged.append(f"{name} › {job_id}")
            found = check_job(name, job_id, job, wf)
            steps = _steps(job)
            visited.add((name, job_id))
            for code, step, reason in EXCEPTIONS.get((name, job_id), ()):
                hit = next((f for f in found if f.code == code and not f.waived and 0 <= f.step < len(steps)
                            and steps[f.step] == step), None)
                if hit:
                    hit.waived = reason
                else:
                    stale.append(f"{name} › {job_id}: {code} on {step}")
            findings += found
    stale += [f"{wf} › {job}: no such job" for wf, job in EXCEPTIONS if (wf, job) not in visited]
    return findings, judged, stale


def report(workflows: pathlib.Path) -> int:
    try:
        findings, judged, stale = check(workflows)
    except YamlError as err:
        print(f"check-publish-guards: cannot judge — {err}", file=sys.stderr)
        return 2
    annotate = bool(os.environ.get("GITHUB_ACTIONS"))
    print(f"privileged jobs ({len(judged)}): {', '.join(judged) or 'none'}")
    for finding in findings:
        where = f"{finding.workflow} › {finding.job}"
        step = f" (step {finding.step + 1})" if finding.step >= 0 else ""
        if finding.waived:
            print(f"  waived  {where} [{finding.code}]{step} {finding.message} — named exception: {finding.waived}")
            continue
        print(f"  FAIL    {where} [{finding.code}]{step} {finding.message}")
        if annotate:
            print(f"::error file=.github/workflows/{finding.workflow},title=publish guard::{where}: {finding.message}")
    for entry in stale:
        print(f"  FAIL    stale named exception {entry}: nothing left to waive, delete it")
    failed = [f for f in findings if not f.waived]
    print(f"{len(failed)} violation(s) · {len(findings) - len(failed)} waived by the named exception · "
          f"{len(stale)} stale exception(s)")
    return 1 if failed or stale else 0


# ---------------------------------------------------------------- toolchain policy


def _msrv(root: pathlib.Path):
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    return manifest.get("workspace", {}).get("package", {}).get("rust-version")


def _same_version(a, b) -> bool:
    def norm(v):
        return re.sub(r"(\.0)+$", "", str(v).strip().strip("\"'"))
    return b is not None and norm(a) == norm(b)


def toolchain_files(root: pathlib.Path) -> list:
    """Every rust-toolchain(.toml) under ROOT: git's view (tracked and
    untracked, not ignored) when ROOT is a work tree's top, else the files."""
    top = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=root, capture_output=True, text=True)
    if top.returncode == 0 and pathlib.Path(top.stdout.strip()).resolve() == root.resolve():
        listed = subprocess.run(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=root,
                                capture_output=True, check=True).stdout.decode().split("\0")
    else:
        listed = [str(path.relative_to(root)) for path in root.rglob("*") if path.is_file()]
    return sorted(name for name in listed if pathlib.PurePosixPath(name).name in ("rust-toolchain",
                                                                                  "rust-toolchain.toml"))


def check_toolchains(root: pathlib.Path, workflows: pathlib.Path) -> tuple:
    """(findings, the RUST_TOOLCHAIN values seen); a YamlError propagates."""
    findings, pins, msrv = [], {}, _msrv(root)

    def flag(workflow: str, job: str, code: str, message: str) -> None:
        findings.append(Finding(workflow, job, code, message))

    for name, wf in load_workflows(workflows):
        envs = [("(workflow)", wf.get("env"))]
        for job_id, job in wf["jobs"].items():
            job = job if isinstance(job, dict) else {}
            envs.append((job_id, job.get("env")))
            for i, step in enumerate(_steps(job)):
                where = f"{job_id} (step {i + 1})"
                envs.append((where, step.get("env")))
                action, ref = _uses(step)
                inputs = step.get("with") if isinstance(step.get("with"), dict) else {}
                toolchain = inputs.get("toolchain")
                named = toolchain is not None and re.sub(r"\s+", "", str(toolchain)) in NAMED_INPUTS
                if action == "dtolnay/rust-toolchain":
                    if re.fullmatch(r"\d+\.\d+(\.\d+)?", ref):
                        if not _same_version(ref, msrv) or (toolchain is not None and not _same_version(toolchain,
                                                                                                    msrv)):
                            flag(name, where, "leg", f"rust-toolchain@{ref}: the one literal is the MSRV leg, at "
                                                     f"the rust-version {msrv} the root manifest declares")
                    elif re.fullmatch(r"(stable|beta|nightly)([-.].*)?", ref):
                        flag(name, where, "leg", f"rust-toolchain@{ref}: a moving channel; name RUST_TOOLCHAIN")
                    elif toolchain is None:
                        flag(name, where, "leg", f"rust-toolchain@{ref} names no toolchain input: the ref decides "
                                                 "which Rust it installs")
                    elif not named and not (SHA.fullmatch(ref) and _same_version(toolchain, msrv)):
                        flag(name, where, "leg", f"rust-toolchain toolchain {toolchain!r}: name RUST_TOOLCHAIN "
                                                 "(or, SHA-pinned, the MSRV leg's rust-version)")
                for key in ("toolchain", "rust-toolchain"):
                    value = inputs.get(key)
                    if action != "dtolnay/rust-toolchain" and value is not None and \
                            re.sub(r"\s+", "", str(value)) not in NAMED_INPUTS:
                        flag(name, where, "leg", f"{action} {key} {value!r}: name RUST_TOOLCHAIN")
                for line in _code_lines(step.get("run")):
                    if re.search(r"\b(RUST_TOOLCHAIN|FUZZ_TOOLCHAIN|RUSTUP_TOOLCHAIN)=", line):
                        flag(name, where, "script", f"`{line}` assigns a toolchain in a script")
                    elif re.search(r"\bRUSTUP_TOOLCHAIN\b", line):
                        flag(name, where, "leg", f"`{line}`: RUSTUP_TOOLCHAIN overrides every leg")
                    if re.search(r"rust-toolchain(\.toml)?\b", line):
                        flag(name, where, "file", f"`{line}`: a rust-toolchain file outranks `rustup default`")
                    named_args = [m.group(1) for m in re.finditer(
                        r"\brustup\s+(?:toolchain\s+(?:install|add)|install|default|override\s+set|run|update)\s+(\S+)",
                        line)]
                    named_args += [m.group(1) for m in re.finditer(r"--toolchain[= ](\S+)", line)]
                    words = line.split()
                    named_args += [word for previous, word in zip(words, words[1:])
                                   if re.search(r"(?:^|/)(cargo|rustc|rustdoc)$", previous.strip("\"'"))
                                   and word.strip("\"'").startswith("+")]
                    for arg in named_args:
                        if not NAMED_VARIABLE.fullmatch(arg.strip("\"'").lstrip("+").strip("\"'")):
                            flag(name, where, "leg", f"`{line}` names {arg}, not $RUST_TOOLCHAIN or $FUZZ_TOOLCHAIN")
        for where, env in envs:
            if env is None:
                continue
            if not isinstance(env, dict):
                flag(name, where, "pin", f"env {env!r} is not a mapping: no check can read its toolchain")
                continue
            if "RUST_TOOLCHAIN" in env:
                pins.setdefault(str(env["RUST_TOOLCHAIN"]), []).append(f"{name} › {where}")
            if "FUZZ_TOOLCHAIN" in env and not re.fullmatch(r"nightly-\d{4}-\d{2}-\d{2}", str(env["FUZZ_TOOLCHAIN"])):
                flag(name, where, "fuzz-pin", f"FUZZ_TOOLCHAIN {env['FUZZ_TOOLCHAIN']!r} is not a dated "
                                              "nightly-YYYY-MM-DD")
            if "RUSTUP_TOOLCHAIN" in env:
                flag(name, where, "leg", "env RUSTUP_TOOLCHAIN overrides every leg's toolchain")
    if len(pins) != 1:
        flag("(all)", "(all)", "pin", f"RUST_TOOLCHAIN must hold one value across every workflow, got "
                                      f"{ {value: len(places) for value, places in pins.items()} }")
    for value, places in pins.items():
        if not EXACT.fullmatch(value):
            flag("(all)", "(all)", "pin", f"RUST_TOOLCHAIN {value!r} is not an exact x.y.z ({', '.join(places)})")
    for path in toolchain_files(root):
        flag("(tree)", path, "file", "a rust-toolchain file outranks RUST_TOOLCHAIN for every build below it")
    return findings, pins


def report_toolchains(root: pathlib.Path, workflows: pathlib.Path) -> int:
    try:
        findings, pins = check_toolchains(root, workflows)
    except YamlError as err:
        print(f"check-publish-guards --toolchains: cannot judge — {err}", file=sys.stderr)
        return 2
    annotate = bool(os.environ.get("GITHUB_ACTIONS"))
    print(f"RUST_TOOLCHAIN: {', '.join(f'{v} ({len(p)})' for v, p in pins.items()) or 'none'} · "
          f"MSRV leg: {_msrv(root)}")
    for finding in findings:
        print(f"  FAIL    {finding.workflow} › {finding.job} [{finding.code}] {finding.message}")
        if annotate:
            print(f"::error title=one Rust toolchain::{finding.workflow} › {finding.job}: {finding.message}")
    print(f"{len(findings)} violation(s)")
    return 1 if findings else 0


# ---------------------------------------------------------------- self-test

PIN = "e2a55d2ffb04f378e9626c28d38b36d230d1e12f"
CHECKOUT = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1"
GATE_STEP = f"      - run: {GATE_RUN}\n"
PRINT_STEP = """\
      - name: node + npm versions (npm >= 11.5.1 for trusted publishing)
        run: |
          node --version
          v=$(npm --version)
          echo "npm $v"
          if [ "$(printf '%s\\n' 11.5.1 "$v" | sort -V | head -n1)" != 11.5.1 ]; then
            echo "::error::npm $v cannot publish through trusted publishing (needs >= 11.5.1)"
            exit 1
          fi
"""
CRATES_IF = ("    if: startsWith(github.ref, 'refs/tags/v')\n    runs-on: ubuntu-24.04\n    timeout-minutes: 30\n"
             "    environment: crates-io")
PY_IF = "    if: startsWith(github.ref, 'refs/tags/v')\n    needs: [test, inspect]"
IOS_IF = "    if: startsWith(github.ref, 'refs/tags/v')\n    runs-on: ubuntu-24.04\n    environment: release"
RUSTUP_STEP = "      - name: Rust ${{ env.RUST_TOOLCHAIN }} through rustup\n        run: |\n"
REINSPECT = "      - name: re-inspect the release set about to be uploaded\n"
WASM_TAIL = "      # the very tarball build-wasm packed, smoke-tested and inspected\n"
NATIVE_PUBLISH = '                if out=$(npm publish "$tgz" --access public --ignore-scripts 2>&1); then'
WASM_PUBLISH = ('              echo "publishing $name@$version"\n'
                '              if out=$(npm publish "$tgz" --access public --ignore-scripts 2>&1); then')
PYPI = "      - uses: pypa/gh-action-pypi-publish"
FLUTTER_PUBLISH = "      - name: publish (OIDC)\n"
EXTRA_PUBLISHER = """\
name: release
on:
  workflow_dispatch:
permissions:
  contents: read
jobs:
  publish:
    runs-on: ubuntu-24.04
    permissions:
      contents: write
      id-token: write
    steps:
      - uses: actions/checkout@v7
      - uses: someone/some-action@main
      - run: npx some-tool
      - run: npm publish --access public
      - run: gh release create v0 x.zip
"""
SNEAKY_PNPM = """
  sneaky:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v7
      - run: pnpm publish --no-git-checks
        env:
          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}
"""
SNEAKY_CARGO = """
  sneaky:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v7
      - run: |
          cargo \\
            publish -p qrcode-ai-scanner
"""
OIDC_PNPM = """
  sneaky:
    if: startsWith(github.ref, 'refs/tags/v')
    runs-on: ubuntu-24.04
    environment: npm
    concurrency:
      group: publish-${{ github.workflow }}-${{ github.ref }}
      cancel-in-progress: false
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          persist-credentials: false
      - run: python3 scripts/package-inspect.py versions --tag "$GITHUB_REF" --strict --quiet
      - run: pnpm publish --no-git-checks
"""
REUSABLE = """
  elsewhere:
    uses: someone/workflows/.github/workflows/publish.yml@main
    secrets: inherit
"""
ALL_WORKFLOWS = ("ci.yml", "crates-publish.yml", "deep-checks.yml", "flutter.yml", "mobile.yml", "npm-publish.yml",
                 "python.yml", "toolchain-probe.yml")
PIN_LINE = '  RUST_TOOLCHAIN: "1.97.0"\n'

# (name, mode, edits [(workflow, old, new)], extra files {path: text}, exit, needle).
# The real tree passes both modes; each mutated copy must exit as said, with
# the needle printed.
P, T = "publish", "toolchains"
MUTATIONS = (
    # -- the tag guard, raw
    ("tag guard removed", P, [("crates-publish.yml", CRATES_IF, CRATES_IF.split("\n", 1)[1])], {}, 1,
     "crates-publish.yml › publish [guard]"),
    ("tag guard under ||", P, [("python.yml", PY_IF, PY_IF.replace("'refs/tags/v')", "'refs/tags/v') || "
                                "github.event_name == 'workflow_dispatch'"))], {}, 1, "under ||"),
    ("tag guard negated", P, [("mobile.yml", IOS_IF, IOS_IF.replace("if: startsWith(github.ref, 'refs/tags/v')",
                                                                    "if: \"!startsWith(github.ref, 'refs/tags/v')\""))],
     {}, 1, "mobile.yml › ios-release [guard]"),
    ("guard as a literal block, ${{ }} and a newline", P, [("crates-publish.yml", CRATES_IF, CRATES_IF.replace(
        "    if: startsWith(github.ref, 'refs/tags/v')", "    if: |\n      ${{ startsWith(github.ref, 'refs/tags/v') }}"))],
     {}, 1, "crates-publish.yml › publish [guard]"),
    ("guard in ${{ }} followed by && true", P, [("python.yml", PY_IF, PY_IF.replace(
        "if: startsWith(github.ref, 'refs/tags/v')", "if: ${{ startsWith(github.ref, 'refs/tags/v') }} && true"))],
     {}, 1, "python.yml › release [guard]"),
    ("guard in ${{ }} followed by && success()", P, [("python.yml", PY_IF, PY_IF.replace(
        "if: startsWith(github.ref, 'refs/tags/v')", "if: ${{ startsWith(github.ref, 'refs/tags/v') }} && success()"))],
     {}, 1, "python.yml › release [guard]"),
    ("guard in ${{ }} with a trailing space", P, [("mobile.yml", IOS_IF, IOS_IF.replace(
        "if: startsWith(github.ref, 'refs/tags/v')", "if: \"${{ startsWith(github.ref, 'refs/tags/v') }} \""))],
     {}, 1, "mobile.yml › ios-release [guard]"),
    ("guard behind a # the reader and YAML split differently", P, [("crates-publish.yml", CRATES_IF, CRATES_IF.replace(
        "if: startsWith(github.ref, 'refs/tags/v')",
        "if: contains('a \"', 'a') #\" && startsWith(github.ref, 'refs/tags/v')"))], {}, 1,
     "crates-publish.yml › publish [guard]"),
    ("guard && !cancelled(): runs when inspect failed", P, [("python.yml", PY_IF, PY_IF.replace(
        "'refs/tags/v')", "'refs/tags/v') && !cancelled()"))], {}, 1, "calls a status function"),
    ("guard && always()", P, [("crates-publish.yml", CRATES_IF, CRATES_IF.replace(
        "'refs/tags/v')", "'refs/tags/v') && always()"))], {}, 1, "calls a status function"),
    ("guard && ALWAYS(): functions ignore case", P, [("python.yml", PY_IF, PY_IF.replace(
        "'refs/tags/v')", "'refs/tags/v') && ALWAYS()"))], {}, 1, "calls a status function"),
    ("guard && !SUCCESS()", P, [("python.yml", PY_IF, PY_IF.replace(
        "'refs/tags/v')", "'refs/tags/v') && !SUCCESS()"))], {}, 1, "calls a status function"),
    ("guard && ALWAYS() inside one ${{ }}", P, [("python.yml", PY_IF, PY_IF.replace(
        "if: startsWith(github.ref, 'refs/tags/v')", "if: ${{ startsWith(github.ref, 'refs/tags/v') && ALWAYS() }}"))],
     {}, 1, "calls a status function"),
    # -- the inspection job
    ("crates publish no longer needs package", P, [("crates-publish.yml", "  publish:\n    needs: package\n",
                                                    "  publish:\n")], {}, 1, "crates-publish.yml › publish [needs]"),
    ("python release needs test only", P, [("python.yml", PY_IF, PY_IF.replace("[test, inspect]", "[test]"))], {}, 1,
     "python.yml › release [needs]"),
    ("a publishing job with no declared inspection job", P, [("npm-publish.yml", None, OIDC_PNPM)], {}, 1,
     "npm-publish.yml › sneaky [needs]"),
    # -- environment, permissions
    ("environment removed", P, [("crates-publish.yml", "    environment: crates-io\n", "")], {}, 1,
     "crates-publish.yml › publish [environment]"),
    ("environment renamed pub.dev", P, [("flutter.yml", "    environment: pub\n", "    environment: pub.dev\n")],
     {}, 1, "[environment]"),
    ("environment by expression", P, [("crates-publish.yml", "    environment: crates-io\n",
                                       "    environment: ${{ 'crates-io' }}\n")], {}, 1, "[environment]"),
    ("packages: write added", P, [("crates-publish.yml", "      id-token: write # crates.io trusted publishing (OIDC)",
                                   "      id-token: write # crates.io trusted publishing (OIDC)\n      packages: write")],
     {}, 1, "`packages: write`"),
    ("contents: write without gh release", P, [("python.yml", "      contents: read\n      id-token: write # trusted "
                                                "publishing", "      contents: write\n      id-token: write # trusted "
                                                "publishing")], {}, 1, "`contents: write`"),
    ("id-token on the GitHub-release job", P, [("mobile.yml", "      contents: write # create the GitHub release + "
                                                "attach the asset", "      contents: write # create the GitHub "
                                                "release + attach the asset\n      id-token: write")], {}, 1,
     "`id-token: write`"),
    ("job permissions write-all", P, [("flutter.yml", "    permissions:\n      contents: read\n      id-token: write "
                                       "# OIDC — pub.dev automated publishing, no token", "    permissions: write-all")],
     {}, 1, "an explicit map"),
    ("id-token inherited from the workflow", P, [("crates-publish.yml", "permissions:\n  contents: read\n",
                                                  "permissions:\n  contents: read\n  id-token: write\n")], {}, 1,
     "top-level permissions must be read-only"),
    ("top-level permissions widened", P, [("deep-checks.yml", "permissions:\n  contents: read\n",
                                           "permissions:\n  contents: write\n")], {}, 1,
     "top-level permissions must be read-only"),
    ("id-token on an unguarded build job", P, [("ci.yml", "  lint:\n    runs-on: ubuntu-24.04\n",
                                                "  lint:\n    runs-on: ubuntu-24.04\n    permissions:\n"
                                                "      id-token: write\n")], {}, 1, "ci.yml › lint [guard]"),
    # -- secrets, anywhere
    ("a registry secret read", P, [("npm-publish.yml", "          name: npm-native-tarballs\n          path: npm-release",
                                    "          name: npm-native-tarballs\n          path: npm-release\n        env:\n"
                                    "          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}")], {}, 1,
     "npm-publish.yml › publish-native [secrets]"),
    ("a secret by index syntax", P, [("npm-publish.yml", REINSPECT, REINSPECT + "        env:\n          "
                                      "NODE_AUTH_TOKEN: ${{ secrets['NPM_TOKEN'] }}\n")], {}, 1,
     "npm-publish.yml › publish-native [secrets]"),
    ("every secret through toJSON", P, [("crates-publish.yml", "          CARGO_REGISTRY_TOKEN: ${{ "
                                         "steps.auth.outputs.token }}", "          CARGO_REGISTRY_TOKEN: ${{ "
                                         "steps.auth.outputs.token }}\n          ALL: ${{ toJSON(secrets) }}")], {}, 1,
     "crates-publish.yml › publish [secrets]"),
    ("a secret in the workflow env", P, [("npm-publish.yml", "env:\n  CARGO_TERM_COLOR: always\n",
                                          "env:\n  CARGO_TERM_COLOR: always\n  NODE_AUTH_TOKEN: ${{ "
                                          "secrets.NPM_TOKEN }}\n")], {}, 1, "npm-publish.yml › (workflow) [secrets]"),
    ("a secret in the workflow env, the context in capitals", P, [(
        "npm-publish.yml", "env:\n  CARGO_TERM_COLOR: always\n",
        "env:\n  CARGO_TERM_COLOR: always\n  NODE_AUTH_TOKEN: ${{ SECRETS.NPM_TOKEN }}\n")], {}, 1,
     "npm-publish.yml › (workflow) [secrets]"),
    ("every secret through toJSON(SECRETS)", P, [("crates-publish.yml", "          CARGO_REGISTRY_TOKEN: ${{ "
                                                  "steps.auth.outputs.token }}", "          CARGO_REGISTRY_TOKEN: ${{ "
                                                  "steps.auth.outputs.token }}\n          ALL: ${{ toJSON(SECRETS) }}")],
     {}, 1, "crates-publish.yml › publish [secrets]"),
    ("every secret handed on by a key spelled Secrets", P, [("crates-publish.yml", None, "\n  elsewhere:\n    uses: "
                                                             "./.github/workflows/ci.yml\n    Secrets: inherit\n")],
     {}, 1, "crates-publish.yml › elsewhere [secrets]"),
    ("a job publishing with pnpm and a secret", P, [("npm-publish.yml", None, SNEAKY_PNPM)], {}, 1,
     "npm-publish.yml › sneaky [secrets]"),
    ("a reusable workflow given every secret", P, [("crates-publish.yml", None, REUSABLE)], {}, 1,
     "crates-publish.yml › elsewhere [reusable-workflow]"),
    # -- publishing spellings: the job becomes privileged and fails
    ("cargo publish split over a continuation", P, [("crates-publish.yml", None, SNEAKY_CARGO)], {}, 1,
     "crates-publish.yml › sneaky [guard]"),
    ("npm options before the subcommand, in a ci job", P, [("ci.yml", "      - run: node test.mjs\n",
                                                            "      - run: node test.mjs\n      - run: npm --access "
                                                            "public publish\n")], {}, 1, "ci.yml › node-smoke [guard]"),
    ("cargo options before the subcommand, in ci › packaging", P, [(
        "ci.yml", "      - run: python3 scripts/package-inspect.py crates --strict --quiet\n",
        "      - run: python3 scripts/package-inspect.py crates --strict --quiet\n      - run: cargo --locked publish "
        "-p qrcode-ai-scanner\n")], {}, 1, "ci.yml › packaging [guard]"),
    ("npm's abbreviation `npm pu`, in a ci job", P, [("ci.yml", "      - run: node test.mjs\n",
                                                      "      - run: node test.mjs\n      - run: npm pu ./x.tgz "
                                                      "--ignore-scripts\n")], {}, 1, "ci.yml › node-smoke [guard]"),
    ("a release upload in a build job", P, [("mobile.yml", "        run: gradle :qrcodeaiscanner:assembleRelease",
                                             "        run: gradle :qrcodeaiscanner:assembleRelease && gh release "
                                             "upload v0 x.aar")], {}, 1, "mobile.yml › android [guard]"),
    ("napi pre-publish that uploads", P, [("npm-publish.yml",
                                           "pnpm exec napi pre-publish -t npm --no-gh-release --skip-optional-publish",
                                           "pnpm exec napi pre-publish -t npm --no-gh-release")], {}, 1,
     "npm-publish.yml › pack-native [guard]"),
    # -- files and reusable workflows
    ("an unguarded publisher in a .yaml workflow", P, [], {".github/workflows/release.yaml": EXTRA_PUBLISHER}, 1,
     "release.yaml › publish [guard]"),
    ("another file in the workflows directory", P, [], {".github/workflows/notes.txt": "x\n"}, 1, "[workflow-file]"),
    # -- pinned code
    ("third-party action by tag", P, [("crates-publish.yml",
                                       "rust-lang/crates-io-auth-action@c6f97d42243bad5fab37ca0427f495c86d5b1a18",
                                       "rust-lang/crates-io-auth-action@v1")], {}, 1,
     "rust-lang/crates-io-auth-action@v1 is not pinned"),
    ("dtolnay@master back in a publish job", P, [("crates-publish.yml", "        run: |\n          rustup toolchain "
                                                   "install \"$RUST_TOOLCHAIN\" --profile minimal\n          rustup "
                                                   "default \"$RUST_TOOLCHAIN\"\n          cargo --version",
                                                   "        uses: dtolnay/rust-toolchain@master\n        with:\n"
                                                   "          toolchain: ${{ env.RUST_TOOLCHAIN }}")], {}, 1,
     "dtolnay/rust-toolchain@master is not pinned"),
    ("checkout by tag in a publish job", P, [("crates-publish.yml", CHECKOUT, "actions/checkout@v7")], {}, 1,
     "crates-publish.yml › publish [unpinned-action]"),
    ("download-artifact by tag in a publish job", P, [(
        "python.yml", "actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2",
        "actions/download-artifact@v8")], {}, 1, "python.yml › release [unpinned-action]"),
    ("a floating first-party action asking for the OIDC token", P, [(
        "npm-publish.yml", REINSPECT, "      - uses: actions/github-script@v8\n        with:\n          script: "
                                      "core.info(await core.getIDToken())\n" + REINSPECT)], {}, 1,
     "npm-publish.yml › publish-native [unpinned-action]"),
    ("checkout persisting the job token", P, [("mobile.yml", f"      - uses: {CHECKOUT}\n        with:\n          "
                                               "persist-credentials: false\n", f"      - uses: {CHECKOUT}\n")], {}, 1,
     "mobile.yml › ios-release [persist-credentials]"),
    ("a local composite action", P, [("npm-publish.yml", WASM_TAIL, "      - uses: ./.github/actions/prepare\n"
                                      + WASM_TAIL)], {}, 1, "npm-publish.yml › publish-wasm [local-action]"),
    ("the publish job in a floating container", P, [("crates-publish.yml", "    environment: crates-io\n",
                                                     "    environment: crates-io\n    container: rust:1\n")], {}, 1,
     "crates-publish.yml › publish [container]"),
    ("a service image by tag", P, [("python.yml", "    environment: release\n", "    environment: release\n"
                                    "    services:\n      cache:\n        image: redis:7\n")], {}, 1,
     "python.yml › release [container]"),
    ("scripts through another shell", P, [("crates-publish.yml", "  publish:\n    needs: package\n",
                                           "  publish:\n    needs: package\n    defaults:\n      run:\n"
                                           "        shell: \"true {0}\"\n")], {}, 1, "crates-publish.yml › publish [shell]"),
    ("an expression in a publish script", P, [("mobile.yml", "          zip=build/QrcodeAiScannerFFI.xcframework.zip\n",
                                               "          zip=${{ github.event.inputs.zip }}\n")], {}, 1,
     "mobile.yml › ios-release [expression]"),
    # -- the versions gate, exactly
    ("versions gate removed", P, [("mobile.yml", GATE_STEP + "      - uses: actions/download-artifact@",
                                   "      - uses: actions/download-artifact@")], {}, 1,
     "mobile.yml › ios-release [gate]"),
    ("versions gate || true", P, [("crates-publish.yml", GATE_STEP + "      # This job can mint",
                                   GATE_STEP.replace("--quiet", "--quiet || true") + "      # This job can mint")],
     {}, 1, "crates-publish.yml › publish [gate]"),
    ("versions gate with continue-on-error", P, [("crates-publish.yml", GATE_STEP + "      # This job can mint",
                                                  GATE_STEP + "        continue-on-error: true\n      # This job can "
                                                  "mint")], {}, 1, "crates-publish.yml › publish [gate]"),
    ("versions gate as a comment", P, [("npm-publish.yml", GATE_STEP + "      # Node 24 for npm",
                                        "      - run: \"true # package-inspect.py versions --tag\"\n      # Node 24 "
                                        "for npm")], {}, 1, "npm-publish.yml › publish-native [gate]"),
    # -- runtimes, printed
    ("setup-node by tag", P, [("npm-publish.yml", "actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1 # v7.1.0",
                               "actions/setup-node@v7")], {}, 1, "needs the action pinned by SHA"),
    *((f"setup-node {spec}", P, [("npm-publish.yml", "node-version: 24\n          registry-url",
                                  f"node-version: {spec}\n          registry-url")], {}, 1,
       "a release line or an exact version")
      for spec in ("lts/*", "latest", "current", "node", "'>=24'", "24.x", "''")),
    ("node versions not printed", P, [("npm-publish.yml", PRINT_STEP + "      - uses: actions/download-artifact@"
                                       "9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2\n        with:\n          "
                                       "name: npm-native-tarballs", "      - uses: actions/download-artifact@"
                                       "9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2\n        with:\n          "
                                       "name: npm-native-tarballs")], {}, 1, "npm-publish.yml › publish-native "
                                                                             "[versions-printed]"),
    ("node versions printed by a comment", P, [("npm-publish.yml", PRINT_STEP + WASM_TAIL,
                                                "      - run: \"true # node --version npm --version\"\n" + WASM_TAIL)],
     {}, 1, "npm-publish.yml › publish-wasm [versions-printed]"),
    ("the npm floor assertion dropped", P, [("npm-publish.yml", PRINT_STEP + WASM_TAIL,
                                             "      - name: node + npm versions\n        run: |\n          node "
                                             "--version\n          npm --version\n" + WASM_TAIL)], {}, 1,
     "npm-publish.yml › publish-wasm [versions-printed]"),
    # -- toolchains and tools in a publish job
    ("rustup on a channel", P, [("crates-publish.yml", 'rustup default "$RUST_TOOLCHAIN"', "rustup default stable")],
     {}, 1, "[floating-rust]"),
    ("RUST_TOOLCHAIN on a channel for the publish job", P, [("crates-publish.yml", "  publish:\n    needs: package\n",
                                                             "  publish:\n    needs: package\n    env:\n      "
                                                             "RUST_TOOLCHAIN: stable\n")], {}, 1,
     "crates-publish.yml › publish [floating-rust]"),
    ("RUST_TOOLCHAIN on a channel for the rustup step", P, [("crates-publish.yml", RUSTUP_STEP, RUSTUP_STEP.replace(
        "        run: |\n", "        env:\n          RUST_TOOLCHAIN: stable\n        run: |\n"))], {}, 1,
     "crates-publish.yml › publish [floating-rust]"),
    ("RUST_TOOLCHAIN written to GITHUB_ENV in a publish job", P, [("crates-publish.yml", RUSTUP_STEP,
                                                                   "      - run: echo \"RUST_TOOLCHAIN=1.96.0\" >> "
                                                                   "\"$GITHUB_ENV\"\n" + RUSTUP_STEP)], {}, 1,
     "crates-publish.yml › publish [floating-rust]"),
    ("a rust-toolchain.toml written in a publish job", P, [(
        "crates-publish.yml", "      - uses: rust-lang/crates-io-auth-action",
        "      - run: printf '[toolchain]\\nchannel = \"stable\"\\n' > rust-toolchain.toml\n"
        "      - uses: rust-lang/crates-io-auth-action")], {}, 1, "crates-publish.yml › publish [floating-rust]"),
    ("pnpm by major line in a publish job", P, [("npm-publish.yml", REINSPECT, "      - uses: pnpm/action-setup@"
                                                 "0977fd99725f1db4007ccb2928dbb4e90d06cc86 # v6.0.10\n        with:\n"
                                                 "          version: 10\n" + REINSPECT)], {}, 1, "[floating-pnpm]"),
    ("setup-go by tag on stable", P, [("python.yml", PYPI, "      - uses: actions/setup-go@v6\n        with:\n"
                                       "          go-version: stable\n" + PYPI)], {}, 1,
     "python.yml › release [floating-tool]"),
    ("setup-uv by SHA on latest", P, [("python.yml", PYPI, "      - uses: astral-sh/setup-uv@"
                                       "0123456789abcdef0123456789abcdef01234567\n        with:\n          version: "
                                       "latest\n" + PYPI)], {}, 1, "python.yml › release [floating-tool]"),
    ("install-action fetching an unversioned tool", P, [(
        "crates-publish.yml", "      - uses: rust-lang/crates-io-auth-action",
        "      - uses: taiki-e/install-action@0000000000000000000000000000000000000001 # v2\n        with:\n"
        "          tool: cargo-release\n      - uses: rust-lang/crates-io-auth-action")], {}, 1,
     "crates-publish.yml › publish [floating-tool]"),
    # -- unlocked code in a publish job
    ("pnpm install in a publish job", P, [("npm-publish.yml", REINSPECT, "      - run: pnpm install "
                                           "--frozen-lockfile=false\n" + REINSPECT)], {}, 1,
     "pnpm without --frozen-lockfile"),
    ("npx in a publish job", P, [("npm-publish.yml", REINSPECT, "      - run: npx some-tool\n" + REINSPECT)], {}, 1,
     "fetches a package on demand"),
    ("npm x in a publish job", P, [("npm-publish.yml", WASM_TAIL, "      - run: npm x --yes some-tool\n" + WASM_TAIL)],
     {}, 1, "npm-publish.yml › publish-wasm [unlocked-install]"),
    ("corepack in a publish job", P, [("npm-publish.yml", REINSPECT, "      - run: corepack enable\n" + REINSPECT)],
     {}, 1, "npm-publish.yml › publish-native [unlocked-install]"),
    ("pip install in a publish job", P, [("python.yml", PYPI, "      - run: pip install twine\n" + PYPI)], {}, 1,
     "pip without --require-hashes"),
    ("python3.12 -m pip install in a publish job", P, [("python.yml", PYPI, "      - run: python3.12 -m pip install "
                                                        "twine\n" + PYPI)], {}, 1, "python.yml › release "
                                                                                    "[unlocked-install]"),
    ("uvx in a publish job", P, [("python.yml", PYPI, "      - run: uvx some-tool\n" + PYPI)], {}, 1,
     "python.yml › release [unlocked-install]"),
    ("cargo +toolchain install in a publish job", P, [(
        "crates-publish.yml", "      - uses: rust-lang/crates-io-auth-action",
        "      - run: cargo +\"$RUST_TOOLCHAIN\" install cargo-release\n      - uses: rust-lang/crates-io-auth-action")],
     {}, 1, "crates-publish.yml › publish [unlocked-install]"),
    ("piped installer in a publish job", P, [("mobile.yml", "      - name: publish + verify the xcframework asset",
                                              "      - run: curl -sSf https://example.invalid/install.sh | sh\n"
                                              "      - name: publish + verify the xcframework asset")], {}, 1,
     "pipes a download into an interpreter"),
    # -- what a publish step uploads
    ("cargo publish verifying again", P, [("crates-publish.yml", 'cargo publish -p "$crate" --locked --no-verify',
                                           'cargo publish -p "$crate" --locked')], {}, 1, "[cargo-publish]"),
    ("cargo publish unlocked", P, [("crates-publish.yml", 'cargo publish -p "$crate" --locked --no-verify',
                                    'cargo publish -p "$crate" --no-verify')], {}, 1, "[cargo-publish]"),
    ("cargo publish verifying, options first", P, [("crates-publish.yml",
                                                    'cargo publish -p "$crate" --locked --no-verify',
                                                    'cargo --locked publish -p "$crate"')], {}, 1, "[cargo-publish]"),
    ("npm publish of a directory", P, [("npm-publish.yml", WASM_PUBLISH, WASM_PUBLISH.replace(
        'npm publish "$tgz"', "cd npm-wasm && npm publish"))], {}, 1, "npm-publish.yml › publish-wasm [npm-publish]"),
    ("npm publish without --ignore-scripts", P, [("npm-publish.yml", NATIVE_PUBLISH, NATIVE_PUBLISH.replace(
        " --ignore-scripts", ""))], {}, 1, "npm-publish.yml › publish-native [npm-publish]"),
    ("pnpm publish in a publish job", P, [("npm-publish.yml", NATIVE_PUBLISH, NATIVE_PUBLISH.replace(
        "npm publish", "pnpm publish"))], {}, 1, "npm-publish.yml › publish-native [npm-publish]"),
    # -- the required words hidden in a trailing shell comment (a run: | block,
    #    where YAML keeps the `#`); the stripper must drop the comment first
    ("npm directory publish, the tarball + flag in a trailing comment", P, [("npm-publish.yml", WASM_PUBLISH,
        WASM_PUBLISH.replace('if out=$(npm publish "$tgz" --access public --ignore-scripts 2>&1); then',
                             'if out=$(npm publish ./wasm-pkg --access public 2>&1); then # "$tgz" --ignore-scripts'))],
     {}, 1, "npm-publish.yml › publish-wasm [npm-publish]"),
    ("npm publish whose operand is a directory, tarball named later", P, [("npm-publish.yml", NATIVE_PUBLISH,
        NATIVE_PUBLISH.replace('npm publish "$tgz" --access public --ignore-scripts',
                               'npm publish crates/qrcode-ai-scanner-node --access public x.tgz --ignore-scripts'))],
     {}, 1, "npm-publish.yml › publish-native [npm-publish]"),
    ("cargo publish with --locked --no-verify in a trailing comment", P, [("crates-publish.yml",
        'cargo publish -p "$crate" --locked --no-verify', 'cargo publish -p "$crate" # --locked --no-verify')], {}, 1,
     "crates-publish.yml › publish [cargo-publish]"),
    ("cargo install in a run block, --locked --version in a trailing comment", P, [("crates-publish.yml",
        "          cargo --version\n",
        "          cargo --version\n          cargo install cargo-release # --locked --version 0.25.0\n")], {}, 1,
     "crates-publish.yml › publish [unlocked-install]"),
    ("pip install in a run block, --require-hashes in a trailing comment", P, [("python.yml", PYPI,
        "      - run: |\n          pip install twine # --require-hashes\n" + PYPI)], {}, 1,
     "python.yml › release [unlocked-install]"),
    ("pnpm install in a run block, --frozen-lockfile in a trailing comment", P, [("npm-publish.yml", REINSPECT,
        "      - run: |\n          pnpm install # --frozen-lockfile\n" + REINSPECT)], {}, 1,
     "npm-publish.yml › publish-native [unlocked-install]"),
    # -- the named exception: three exact steps, nothing else
    ("the Flutter waiver claimed by another job", P, [("python.yml", PYPI, "      - run: flutter pub get\n" + PYPI)],
     {}, 1, "FAIL    python.yml › release [unlocked-pub-get]"),
    ("the Flutter job with a third violation", P, [("flutter.yml", "    concurrency:\n      group: publish-${{ "
                                                    "github.workflow }}-${{ github.ref }}\n      cancel-in-progress: "
                                                    "false\n", "")], {}, 1, "FAIL    flutter.yml › publish "
                                                                            "[concurrency]"),
    ("flutter pub add beside the waived pub get", P, [("flutter.yml", FLUTTER_PUBLISH, "      - run: flutter pub add "
                                                       "some_package\n" + FLUTTER_PUBLISH)], {}, 1,
     "FAIL    flutter.yml › publish [unlocked-pub-get]"),
    ("a second waived-looking pub get", P, [("flutter.yml", FLUTTER_PUBLISH, "      - run: flutter pub get\n"
                                             + FLUTTER_PUBLISH)], {}, 1, "FAIL    flutter.yml › publish "
                                                                         "[unlocked-pub-get]"),
    ("a second Flutter SDK from the master channel", P, [(
        "flutter.yml", FLUTTER_PUBLISH, "      - uses: subosito/flutter-action@e938fdf56512cc96ef2f93601a5a40bde3801046"
                                        "\n        with:\n          channel: master\n" + FLUTTER_PUBLISH)], {}, 1,
     "FAIL    flutter.yml › publish [floating-flutter-sdk]"),
    ("a globally activated package in the Flutter job", P, [("flutter.yml", FLUTTER_PUBLISH, "      - run: dart pub "
                                                             "global activate some_tool\n" + FLUTTER_PUBLISH)], {}, 1,
     "FAIL    flutter.yml › publish [unlocked-install]"),
    ("dart run in the Flutter job", P, [("flutter.yml", FLUTTER_PUBLISH, "      - run: dart run build_runner build\n"
                                         + FLUTTER_PUBLISH)], {}, 1, "FAIL    flutter.yml › publish [unlocked-install]"),
    ("the Flutter exception gone stale", P, [("flutter.yml", "      - uses: dart-lang/setup-dart@6afc89df92d6eb3834022f7"
                                              "3cd65adc8cdfcb92d # v1.8.1\n      - uses: subosito/flutter-action@e938f"
                                              "df56512cc96ef2f93601a5a40bde3801046 # v2.19.0\n        with:\n          "
                                              "channel: stable\n      - run: flutter pub get\n",
                                              "      - uses: dart-lang/setup-dart@6afc89df92d6eb3834022f73cd65adc8cdfc"
                                              "b92d # v1.8.1\n        with:\n          sdk: 3.10.4\n      - uses: "
                                              "subosito/flutter-action@e938fdf56512cc96ef2f93601a5a40bde3801046 # "
                                              "v2.19.0\n        with:\n          flutter-version: 3.44.2\n      - run: "
                                              "flutter pub get --enforce-lockfile\n")], {}, 1,
     "stale named exception flutter.yml › publish"),
    # -- the concurrency group
    ("concurrency removed", P, [("python.yml", "    concurrency:\n      group: publish-${{ github.workflow }}-${{ "
                                 "github.ref }}\n      cancel-in-progress: false\n    permissions:", "    permissions:")],
     {}, 1, "python.yml › release [concurrency]"),
    ("concurrency cancelling", P, [("mobile.yml", "      cancel-in-progress: false\n    permissions:\n      contents: "
                                    "write", "      cancel-in-progress: true\n    permissions:\n      contents: write")],
     {}, 1, "mobile.yml › ios-release [concurrency]"),
    # -- the reader refuses what it cannot read
    ("an anchor the reader refuses", P, [("toolchain-probe.yml", "    runs-on: ubuntu-24.04\n",
                                          "    runs-on: &runner ubuntu-24.04\n")], {}, 2, "anchors, aliases and tags"),
    ("a comment inside a flow collection", P, [("ci.yml", "        os: [ubuntu-24.04, macos-latest, windows-latest]",
                                                "        os: [ubuntu-24.04, macos-latest # x\n          , "
                                                "windows-latest]")], {}, 2, "cannot judge"),
    # == the one-toolchain policy
    ("RUST_TOOLCHAIN on stable everywhere", T, [(wf, PIN_LINE, "  RUST_TOOLCHAIN: stable\n") for wf in ALL_WORKFLOWS],
     {}, 1, "RUST_TOOLCHAIN 'stable' is not an exact x.y.z"),
    ("RUST_TOOLCHAIN at 1.97 everywhere", T, [(wf, PIN_LINE, '  RUST_TOOLCHAIN: "1.97"\n') for wf in ALL_WORKFLOWS],
     {}, 1, "RUST_TOOLCHAIN '1.97' is not an exact x.y.z"),
    ("a second pin", T, [("npm-publish.yml", PIN_LINE, '  RUST_TOOLCHAIN: "1.98.0"\n')], {}, 1,
     "must hold one value"),
    ("a job-level RUST_TOOLCHAIN", T, [("npm-publish.yml", "  build-native:\n    strategy:\n", "  build-native:\n    "
                                        "env:\n      RUST_TOOLCHAIN: \"1.96.0\"\n    strategy:\n")], {}, 1,
     "must hold one value"),
    ("a step-level RUST_TOOLCHAIN", T, [("crates-publish.yml", RUSTUP_STEP, RUSTUP_STEP.replace(
        "        run: |\n", "        env:\n          RUST_TOOLCHAIN: \"1.96.0\"\n        run: |\n"))], {}, 1,
     "must hold one value"),
    ("RUST_TOOLCHAIN written to GITHUB_ENV", T, [("ci.yml", "      - uses: taiki-e/install-action@nextest\n",
                                                  "      - run: echo \"RUST_TOOLCHAIN=1.96.0\" >> \"$GITHUB_ENV\"\n"
                                                  "      - uses: taiki-e/install-action@nextest\n")], {}, 1,
     "ci.yml › test (step 4) [script]"),
    ("a literal toolchain in a flow mapping", T, [("flutter.yml", "      - uses: dtolnay/rust-toolchain@master\n"
                                                   "        with:\n          toolchain: ${{ env.RUST_TOOLCHAIN }}\n",
                                                   "      - uses: dtolnay/rust-toolchain@master\n        with: { "
                                                   "toolchain: \"1.96.0\" }\n")], {}, 1, "flutter.yml › test (step 2) "
                                                                                         "[leg]"),
    ("a literal beside the variable's name", T, [("crates-publish.yml", 'rustup default "$RUST_TOOLCHAIN"',
                                                  "rustup default 1.96.0 # not $RUST_TOOLCHAIN")], {}, 1,
     "names 1.96.0"),
    ("a literal through cargo +", T, [("ci.yml", "cargo run --locked -p xtask -- corpus-report | tee",
                                       "cargo +1.96.0 run --locked -p xtask -- corpus-report | tee")], {}, 1,
     "names +1.96.0"),
    ("a literal through --toolchain", T, [("ci.yml", "      - run: cargo fmt --all --check\n",
                                           "      - run: rustup component add clippy --toolchain 1.96.0\n      - run: "
                                           "cargo fmt --all --check\n")], {}, 1, "names 1.96.0"),
    ("a moving channel by ref", T, [("ci.yml", "      - uses: dtolnay/rust-toolchain@master\n        with:\n          "
                                     "toolchain: ${{ env.RUST_TOOLCHAIN }}\n          components: rustfmt, clippy\n",
                                     "      - uses: dtolnay/rust-toolchain@stable\n        with:\n          "
                                     "components: rustfmt, clippy\n")], {}, 1, "a moving channel"),
    ("a moving channel by input", T, [("deep-checks.yml", "toolchain: ${{ env.RUST_TOOLCHAIN }}",
                                       "toolchain: stable")], {}, 1, "deep-checks.yml › mutants (step 3) [leg]"),
    ("maturin on a literal toolchain", T, [("python.yml", "rust-toolchain: ${{ env.RUST_TOOLCHAIN }}",
                                            "rust-toolchain: 1.96.0")], {}, 1, "rust-toolchain '1.96.0'"),
    ("RUSTUP_TOOLCHAIN in a workflow env", T, [("ci.yml", "  RUSTFLAGS: -D warnings\n", "  RUSTFLAGS: -D warnings\n"
                                                "  RUSTUP_TOOLCHAIN: 1.97.0\n")], {}, 1, "RUSTUP_TOOLCHAIN overrides"),
    ("the MSRV leg below rust-version", T, [("ci.yml", "dtolnay/rust-toolchain@1.88", "dtolnay/rust-toolchain@1.87")],
     {}, 1, "the one literal is the MSRV leg"),
    ("the MSRV leg by SHA, no toolchain input", T, [("ci.yml", "dtolnay/rust-toolchain@1.88",
                                                     "dtolnay/rust-toolchain@e0000000000000000000000000000000000000a1 "
                                                     "# 1.87.0")], {}, 1, "names no toolchain input"),
    ("the MSRV leg by SHA with its rust-version", T, [("ci.yml", "      - uses: dtolnay/rust-toolchain@1.88\n",
                                                       "      - uses: dtolnay/rust-toolchain@e0000000000000000000000000"
                                                       "000000000000a1 # 1.88\n        with:\n          toolchain: "
                                                       "\"1.88\"\n")], {}, 0, "0 violation(s)"),
    ("rust-toolchain@master pinned by a digit-first SHA", T, [(
        "ci.yml", "      - uses: dtolnay/rust-toolchain@master\n        with:\n          toolchain: ${{ "
                  "env.RUST_TOOLCHAIN }}\n          components: rustfmt, clippy\n",
        "      - uses: dtolnay/rust-toolchain@0f00000000000000000000000000000000000000 # master\n        with:\n"
        "          toolchain: ${{ env.RUST_TOOLCHAIN }}\n          components: rustfmt, clippy\n")], {}, 0,
     "0 violation(s)"),
    ("FUZZ_TOOLCHAIN on the moving nightly", T, [("deep-checks.yml", "  FUZZ_TOOLCHAIN: nightly-2026-08-24\n",
                                                  "  FUZZ_TOOLCHAIN: nightly\n")], {}, 1, "[fuzz-pin]"),
    ("a rust-toolchain.toml at the root", T, [], {"rust-toolchain.toml": '[toolchain]\nchannel = "1.96.0"\n'}, 1,
     "(tree) › rust-toolchain.toml [file]"),
    ("a rust-toolchain file in a crate", T, [], {"crates/qrcode-ai-scanner-py/rust-toolchain": "stable\n"}, 1,
     "crates/qrcode-ai-scanner-py/rust-toolchain [file]"),
    ("a .yaml workflow naming a literal", T, [], {".github/workflows/extra.yaml": (
        "name: extra\non:\n  workflow_dispatch:\npermissions:\n  contents: read\njobs:\n  build:\n    runs-on: "
        "ubuntu-24.04\n    steps:\n      - uses: dtolnay/rust-toolchain@master\n        with:\n          toolchain: "
        "1.96.0\n")}, 1, "extra.yaml › build (step 1) [leg]"),
)


def _run(argv: list) -> tuple:
    sink = io.StringIO()
    saved = os.environ.pop("GITHUB_ACTIONS", None)  # keep scenario annotations out of the real log
    try:
        with contextlib.redirect_stdout(sink), contextlib.redirect_stderr(sink):
            code = main(argv)
    finally:
        if saved is not None:
            os.environ["GITHUB_ACTIONS"] = saved
    return code, sink.getvalue()


def self_test() -> int:
    failures = []
    for mode in (P, T):
        code, out = _run(["--toolchains"] if mode == T else [])
        if code != 0:
            failures.append(f"the real tree ({mode}): exit {code} (want 0)\n{out}")
    for name, mode, edits, extra, want, needle in MUTATIONS:
        with tempfile.TemporaryDirectory(prefix="publish-guards-") as tmp:
            root = pathlib.Path(tmp)
            workflows = root / ".github" / "workflows"
            shutil.copytree(ROOT / ".github" / "workflows", workflows)
            shutil.copyfile(ROOT / "Cargo.toml", root / "Cargo.toml")
            missing = []
            for workflow, old, new in edits:
                text = (workflows / workflow).read_text()
                if old is None:
                    text += new
                elif old in text:
                    text = text.replace(old, new, 1)
                else:
                    missing.append(workflow)
                (workflows / workflow).write_text(text)
            if missing:
                failures.append(f"{name}: anchor not found in {', '.join(missing)} (update the mutation)")
                continue
            for path, text in extra.items():
                (root / path).parent.mkdir(parents=True, exist_ok=True)
                (root / path).write_text(text)
            code, out = _run(["--root", str(root), *(["--toolchains"] if mode == T else [])])
            if code != want or needle not in out:
                failures.append(f"{name}: exit {code} (want {want}), {needle!r} "
                                f"{'present' if needle in out else 'absent'}\n{out}")
    for failure in failures:
        print(f"SELF-TEST FAILED · {failure}", file=sys.stderr)
    total = 2 + len(MUTATIONS)
    print(f"self-test: {total - len(failures)}/{total} scenarios as expected (the real tree passes both policies; "
          f"{len(MUTATIONS)} mutated copies each exit as expected)")
    return 1 if failures else 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--root", type=pathlib.Path, default=ROOT,
                        help="the repository root: Cargo.toml, rust-toolchain files, .github/workflows")
    parser.add_argument("--workflows", type=pathlib.Path, help="the workflows directory (default ROOT/.github/workflows)")
    parser.add_argument("--toolchains", action="store_true", help="judge the one-toolchain policy")
    parser.add_argument("--self-test", action="store_true", help="run the built-in scenarios, then exit")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    workflows = args.workflows or args.root / ".github" / "workflows"
    return report_toolchains(args.root, workflows) if args.toolchains else report(workflows)


if __name__ == "__main__":
    sys.exit(main())
