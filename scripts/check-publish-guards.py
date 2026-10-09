#!/usr/bin/env python3
"""Publishing-job policy for .github/workflows: a static check, run in ci › lint.

A job is PRIVILEGED when it publishes (a step runs `cargo publish`, `npm
publish`, `napi pre-publish`, `pub publish`, `twine upload`, `gh release
create|upload|edit|delete`, pypa/gh-action-pypi-publish or
rust-lang/crates-io-auth-action) or when it holds a write permission
(`id-token: write` included: it can mint registry tokens). Every privileged
job must:

  guard        run from a release tag only: its `if:` carries the top-level
               conjunct startsWith(github.ref, 'refs/tags/v'), never under `||`
  environment  name one of the environments crates-io · npm · release · pub,
               whose deployment rules are the security boundary
  permissions  declare its own, minimal: contents read (write only to run
               `gh release`), id-token write only to publish to a registry
  secrets      read no repository secret
  pinned       pin every third-party action by commit SHA (actions/* are
               first-party), setup-node and setup-python included (below)
  gate         run `package-inspect.py versions --tag` as its first step after
               the checkout: the tag names the version, the CHANGELOG its date
  toolchains   pinned only: Rust through rustup or a SHA-pinned action at
               RUST_TOOLCHAIN; pnpm, Flutter and Dart at an exact x.y.z
  node/python  actions/setup-node and actions/setup-python, SHA-pinned, may
               name a release line (node 24, python 3.12) or an exact version,
               never lts/*, latest, current, node, a range or nothing: security
               point releases arrive without a repository change, and the
               action and its version manifest are first-party. The job prints
               the resolved versions before its first publishing step.
  locked       fetch no unlocked dependency (no npm/pnpm/yarn install, npx,
               pip install, cargo install without --locked, pub get without
               --enforce-lockfile, piped installer); `cargo publish` carries
               --locked --no-verify
  concurrency  join publish-${{ github.workflow }}-${{ github.ref }}, never
               cancelling, so a re-run on the tag waits instead of racing

Every workflow also sets read-only top-level permissions.

`napi pre-publish` with both --skip-optional-publish and --no-gh-release (or
--dry-run) uploads nothing (@napi-rs/cli 3.7.3, cli/src/api/pre-publish.ts):
it only writes optionalDependencies, so it does not make a job privileged.

One NAMED EXCEPTION, scoped to flutter.yml › publish and to exactly two facts:
its Flutter and Dart SDKs come from the stable channel, and `flutter pub get`
resolves without a lockfile. The job is dormant (pub.dev's first upload is
manual and the package's crate still depends on a path outside it), and pub
resolution runs no package code. The exception ends before the pub.dev
trusted publisher is configured (RELEASING.md § Publishing): a waiver that no
longer matches a finding fails this check, so it is removed with the fix.

  check-publish-guards.py [--workflows DIR]   judge the workflows (default .github/workflows)
  check-publish-guards.py --self-test         the real workflows pass, every mutated copy fails

The YAML reader covers the subset the workflows here use (block mappings and
sequences, flow collections, quoted and plain scalars, literal and folded
block scalars, comments) and refuses anything else: anchors, aliases, tags,
multi-line plain scalars. Exit 0 compliant · 1 a violation · 2 cannot judge
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
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"

ENVIRONMENTS = {"crates-io", "npm", "release", "pub"}
GUARD = "startsWith(github.ref, 'refs/tags/v')"
CONCURRENCY = "publish-${{ github.workflow }}-${{ github.ref }}"
GATE = re.compile(r"package-inspect\.py versions --tag\b")
SHA = re.compile(r"[0-9a-f]{40}")
EXACT = re.compile(r"\d+\.\d+\.\d+")

DORMANT = "dormant until pub.dev's manual first upload and the fix of the path dependency"
# (workflow, job) -> {finding code: reason}; nothing else is ever waived.
EXCEPTIONS = {
    ("flutter.yml", "publish"): {
        "floating-flutter-sdk": DORMANT,
        "floating-dart-sdk": DORMANT,
        "unlocked-pub-get": f"{DORMANT}; pub resolution runs no package code",
    },
}

# A step publishes when it runs one of these, or uses one of PUBLISHING_ACTIONS.
PUBLISHING_RUN = (
    (re.compile(r"\bcargo\s+(publish|yank|owner)\b"), "cargo publish"),
    (re.compile(r"\bnpm\s+(publish|unpublish|deprecate|dist-tag)\b"), "npm publish"),
    (re.compile(r"\bnapi\s+pre-?publish\b"), "napi pre-publish"),
    (re.compile(r"\bpub\s+publish\b"), "pub publish"),
    (re.compile(r"\btwine\s+upload\b"), "twine upload"),
    (re.compile(r"\bgh\s+release\s+(create|upload|edit|delete)\b"), "gh release"),
)
PUBLISHING_ACTIONS = ("pypa/gh-action-pypi-publish", "rust-lang/crates-io-auth-action")


# ---------------------------------------------------------------- YAML subset


class YamlError(ValueError):
    pass


_KEY = re.compile(r"""([A-Za-z0-9_][A-Za-z0-9_.-]*|"[^"]*"|'[^']*')[ \t]*:(?:[ \t]+|$)""")
_ESCAPES = {"n": "\n", "t": "\t", "r": "\r", '"': '"', "\\": "\\", "/": "/", "0": "\0", " ": " "}


def _is_dash(content: str) -> bool:
    return content == "-" or content.startswith("- ")


def _strip_comment(text: str) -> str:
    """Drop a trailing ` # comment` that sits outside quotes."""
    quote = None
    for i, ch in enumerate(text):
        if quote:
            if ch == quote:
                quote = None
        elif ch in "'\"" and (i == 0 or text[i - 1] in " \t[{,:"):
            quote = ch
        elif ch == "#" and (i == 0 or text[i - 1] in " \t"):
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
        text = _strip_comment(text)
        if text[:1] in ("&", "*", "!"):
            raise YamlError(f"line {self.i}: anchors, aliases and tags are not supported")
        if not text:
            head = self.peek()
            if head is None:
                return None
            if head[0] > indent:
                return self.node(head[0])
            if head[0] == indent and _is_dash(head[1]):
                return self.sequence(indent)
            return None
        if text[0] in "[{\"'":
            try:
                value, end = _flow(text, 0)
            except IndexError:
                raise YamlError(f"line {self.i}: unterminated {text!r}") from None
            if text[end:].strip():
                raise YamlError(f"line {self.i}: trailing text after {text[:end]!r}")
            return value
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


# ---------------------------------------------------------------- policy


@dataclasses.dataclass
class Finding:
    workflow: str
    job: str
    code: str
    message: str
    waived: str = ""


def _text(node) -> str:
    """Every key and string inside a node, newline-joined."""
    if isinstance(node, dict):
        return "\n".join(f"{k}\n{_text(v)}" for k, v in node.items())
    if isinstance(node, list):
        return "\n".join(_text(v) for v in node)
    return "" if node is None else str(node)


def _code_lines(script: str):
    for line in script.splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            yield line


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
    """An expression without ${{ }}, outer parentheses or extra whitespace."""
    expr = expr.strip()
    if expr.startswith("${{") and expr.endswith("}}"):
        expr = expr[3:-2].strip()
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
    """'' when CONDITION carries the tag guard as a top-level conjunct, else why not."""
    if condition is None:
        return "no `if:`: the job runs on every ref"
    expr = _unwrap(str(condition))
    if len(_top_level(expr, "||")) > 1:
        return f"`if: {condition}` puts the tag guard under ||"
    if GUARD not in (_unwrap(part) for part in _top_level(expr, "&&")):
        return f"`if: {condition}` has no top-level {GUARD}"
    return ""


def _uses(step) -> tuple:
    """(action without its ref, ref) of a `uses:` step, or ('', '')."""
    uses = step.get("uses") if isinstance(step, dict) else None
    if not uses:
        return "", ""
    action, _, ref = str(uses).partition("@")
    return action, ref


def publishing_steps(steps) -> list:
    """(index, what) of every step that publishes."""
    found = []
    for index, step in enumerate(steps):
        action, _ = _uses(step)
        if any(action == a or action.startswith(a + "/") for a in PUBLISHING_ACTIONS):
            found.append((index, action))
            continue
        for line in _code_lines(str(step.get("run") or "")):
            for pattern, what in PUBLISHING_RUN:
                if not pattern.search(line) or "--dry-run" in line:
                    continue
                if what == "napi pre-publish" and "--skip-optional-publish" in line and \
                        re.search(r"--no-gh-release\b|--gh-release[= ]false\b", line):
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
    """(code, message) when a run line fetches an unlocked dependency, else None."""
    if re.search(r"\b(flutter|dart)\s+pub\s+(get|upgrade|add|downgrade)\b", line) and \
            "--enforce-lockfile" not in line:
        return "unlocked-pub-get", f"`{line}` resolves without --enforce-lockfile"
    if re.search(r"\bnpm\s+(install|i|add|update|up|exec)\b", line):
        return "unlocked-install", f"`{line}`: npm without a lockfile (`npm ci` is the locked form)"
    if re.search(r"\bpnpm\s+(install|i|add|update|up)\b", line) and not re.search(r"--frozen-lockfile(\s|$)", line):
        return "unlocked-install", f"`{line}`: pnpm without --frozen-lockfile"
    if re.search(r"\b(npx|pnpx|bunx)\b|\b(pnpm|yarn)\s+dlx\b", line):
        return "unlocked-install", f"`{line}` fetches a package on demand"
    if re.search(r"\byarn\b", line) and not re.search(r"--(immutable|frozen-lockfile)\b", line):
        return "unlocked-install", f"`{line}`: yarn without --immutable"
    if re.search(r"\bpip3?\s+install\b|\bpython3?\s+-m\s+pip\s+install\b|\bpipx\b", line) and \
            "--require-hashes" not in line:
        return "unlocked-install", f"`{line}`: pip without --require-hashes"
    if re.search(r"\bcargo\s+install\b", line) and not (
            "--locked" in line and re.search(r"--version[= ]=?\d+\.\d+\.\d+", line)):
        return "unlocked-install", f"`{line}`: cargo install needs --locked and an exact --version"
    if re.search(r"\b(gem|go)\s+(install|get)\b", line):
        return "unlocked-install", f"`{line}`"
    if re.search(r"\b(curl|wget)\b.*\|\s*(sudo\s+)?(ba|z)?sh\b", line):
        return "unlocked-install", f"`{line}` pipes a download into a shell"
    return None


def _toolchain_findings(step) -> list:
    """Floating-toolchain findings of one step of a privileged job."""
    found = []
    action, ref = _uses(step)
    inputs = step.get("with") or {}
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
    for line in _code_lines(str(step.get("run") or "")):
        for match in re.finditer(r"\brustup\s+(?:toolchain\s+install|default|override\s+set|run)\s+(\S+)", line):
            if match.group(1).strip("\"'{}$") != "RUST_TOOLCHAIN":
                found.append(("floating-rust", f"`{line}` names {match.group(1)}, not $RUST_TOOLCHAIN"))
    return found


# The resolved versions a job relying on the setup-node/-python relaxation prints.
PRINTED = {
    "actions/setup-node": ("`node --version` and `npm --version`",
                           (re.compile(r"\bnode --version\b"), re.compile(r"\bnpm --version\b"))),
    "actions/setup-python": ("`python --version` and `pip --version`",
                             (re.compile(r"\bpython3? --version\b"), re.compile(r"\bpip3? --version\b"))),
}


def check_job(workflow: str, job_id: str, job: dict) -> list:
    steps = job.get("steps") or []
    publishing = publishing_steps(steps)
    if not publishing and not _write_permissions(job.get("permissions")):
        return []
    out: list = []

    def flag(code: str, message: str) -> None:
        out.append(Finding(workflow, job_id, code, message))

    if problem := guarded(job.get("if")):
        flag("guard", problem)
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
    if "secrets." in _text(job):
        flag("secrets", "reads a repository secret")
    for step in steps:
        action, ref = _uses(step)
        if action and not (action.startswith(("./", "docker://", "actions/")) or SHA.fullmatch(ref)):
            flag("unpinned-action", f"{action}@{ref} is not pinned by commit SHA")
        if action.startswith("docker://") and "@sha256:" not in str(step.get("uses")):
            flag("unpinned-action", f"{step.get('uses')} is not pinned by digest")
        for code, message in _toolchain_findings(step):
            flag(code, message)
        for line in _code_lines(str(step.get("run") or "")):
            if hit := _unlocked(line):
                flag(*hit)
            if re.search(r"\bcargo\s+publish\b", line) and not ("--locked" in line and "--no-verify" in line):
                flag("cargo-publish", f"`{line}` needs --locked --no-verify (the package job verified this commit)")
    checkout = next((i for i, step in enumerate(steps) if _uses(step)[0] == "actions/checkout"), None)
    if checkout is None or checkout + 1 >= len(steps) or not GATE.search(str(steps[checkout + 1].get("run") or "")):
        flag("gate", "the first step after the checkout must be `package-inspect.py versions --tag`")
    first_publish = min((i for i, _ in publishing), default=len(steps))
    for runtime, (label, needles) in PRINTED.items():
        if any(_uses(step)[0] == runtime for step in steps) and not any(
                all(n.search(str(step.get("run") or "")) for n in needles) for step in steps[:first_publish]):
            flag("versions-printed", f"{runtime}: print the resolved versions ({label}) in one step "
                                     "before the first publishing step")
    concurrency = job.get("concurrency")
    group = concurrency.get("group") if isinstance(concurrency, dict) else concurrency
    cancels = str(concurrency.get("cancel-in-progress", "false")) if isinstance(concurrency, dict) else "false"
    if re.sub(r"\s+", "", str(group)) != re.sub(r"\s+", "", CONCURRENCY) or cancels != "false":
        flag("concurrency", f"concurrency {concurrency!r}: want group {CONCURRENCY}, cancel-in-progress false")
    return out


def check(workflows: pathlib.Path) -> tuple:
    """(findings, privileged jobs judged, stale waivers); a YamlError propagates."""
    findings, judged, waived = [], [], set()
    for path in sorted(workflows.glob("*.yml")):
        try:
            wf = load_yaml(path.read_text())
        except YamlError as err:
            raise YamlError(f"{path.name}: {err}") from err
        if not isinstance(wf, dict) or not isinstance(wf.get("jobs"), dict):
            raise YamlError(f"{path.name}: no jobs map")
        top = wf.get("permissions")
        if not (isinstance(top, dict) and not _write_permissions(top)) and top != "read-all":
            findings.append(Finding(path.name, "(workflow)", "permissions",
                                    f"top-level permissions must be read-only, got {top!r}"))
        for job_id, job in wf["jobs"].items():
            job = job or {}
            if publishing_steps(job.get("steps") or []) or _write_permissions(job.get("permissions")):
                judged.append(f"{path.name} › {job_id}")
            waivers = EXCEPTIONS.get((path.name, job_id), {})
            for finding in check_job(path.name, job_id, job):
                if finding.code in waivers:
                    finding.waived = waivers[finding.code]
                    waived.add((path.name, job_id, finding.code))
                findings.append(finding)
    stale = [f"{wf} › {job}: {code}" for (wf, job), codes in EXCEPTIONS.items()
             for code in codes if (wf, job, code) not in waived]
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
        if finding.waived:
            print(f"  waived  {where} [{finding.code}] {finding.message} — named exception: {finding.waived}")
            continue
        print(f"  FAIL    {where} [{finding.code}] {finding.message}")
        if annotate:
            print(f"::error file=.github/workflows/{finding.workflow},title=publish guard::{where}: {finding.message}")
    for entry in stale:
        print(f"  FAIL    stale named exception {entry}: nothing left to waive, delete it")
    failed = [f for f in findings if not f.waived]
    print(f"{len(failed)} violation(s) · {len(findings) - len(failed)} waived by the named exception · "
          f"{len(stale)} stale exception(s)")
    return 1 if failed or stale else 0


# ---------------------------------------------------------------- self-test

PRINT_STEP = """\
      - name: node + npm versions (npm >= 11.5.1 for trusted publishing)
        run: |
          node --version
          v=$(npm --version)
"""
# Mutations of the real workflows: (name, file, old, new, expected exit, needle).
# The real tree passes; each mutated copy must fail with the needle printed.
MUTATIONS = (
    ("tag guard removed", "crates-publish.yml", "    if: startsWith(github.ref, 'refs/tags/v')\n    runs-on: ubuntu-24.04\n"
     "    timeout-minutes: 30\n    environment: crates-io", "    runs-on: ubuntu-24.04\n    timeout-minutes: 30\n"
     "    environment: crates-io", 1, "crates-publish.yml › publish [guard]"),
    ("tag guard under ||", "python.yml", "    if: startsWith(github.ref, 'refs/tags/v')\n    needs: [test, inspect]",
     "    if: startsWith(github.ref, 'refs/tags/v') || github.event_name == 'workflow_dispatch'\n"
     "    needs: [test, inspect]", 1, "under ||"),
    ("tag guard negated", "mobile.yml", "    if: startsWith(github.ref, 'refs/tags/v')\n    runs-on: ubuntu-24.04\n"
     "    environment: release", "    if: \"!startsWith(github.ref, 'refs/tags/v')\"\n    runs-on: ubuntu-24.04\n"
     "    environment: release", 1, "mobile.yml › ios-release [guard]"),
    ("environment removed", "crates-publish.yml", "    environment: crates-io\n", "", 1,
     "crates-publish.yml › publish [environment]"),
    ("environment renamed pub.dev", "flutter.yml", "    environment: pub\n", "    environment: pub.dev\n", 1,
     "[environment]"),
    ("packages: write added", "crates-publish.yml", "      id-token: write # crates.io trusted publishing (OIDC)",
     "      id-token: write # crates.io trusted publishing (OIDC)\n      packages: write", 1, "`packages: write`"),
    ("contents: write without gh release", "python.yml", "      contents: read\n      id-token: write # trusted "
     "publishing", "      contents: write\n      id-token: write # trusted publishing", 1, "`contents: write`"),
    ("id-token on the GitHub-release job", "mobile.yml", "      contents: write # create the GitHub release + attach "
     "the asset", "      contents: write # create the GitHub release + attach the asset\n      id-token: write", 1,
     "`id-token: write`"),
    ("job permissions write-all", "flutter.yml", "    permissions:\n      contents: read\n      id-token: write # "
     "OIDC — pub.dev automated publishing, no token", "    permissions: write-all", 1, "an explicit map"),
    ("a registry secret read", "npm-publish.yml", "          name: npm-native-tarballs\n          path: npm-release",
     "          name: npm-native-tarballs\n          path: npm-release\n        env:\n          NODE_AUTH_TOKEN: "
     "${{ secrets.NPM_TOKEN }}", 1, "[secrets]"),
    ("third-party action by tag", "crates-publish.yml",
     "rust-lang/crates-io-auth-action@c6f97d42243bad5fab37ca0427f495c86d5b1a18", "rust-lang/crates-io-auth-action@v1",
     1, "rust-lang/crates-io-auth-action@v1 is not pinned"),
    ("dtolnay@master back in a publish job", "crates-publish.yml", "        run: |\n          rustup toolchain install "
     "\"$RUST_TOOLCHAIN\" --profile minimal\n          rustup default \"$RUST_TOOLCHAIN\"\n          cargo --version",
     "        uses: dtolnay/rust-toolchain@master\n        with:\n          toolchain: ${{ env.RUST_TOOLCHAIN }}", 1,
     "dtolnay/rust-toolchain@master is not pinned"),
    ("rustup on a channel", "crates-publish.yml", 'rustup default "$RUST_TOOLCHAIN"', "rustup default stable", 1,
     "[floating-rust]"),
    ("setup-node by tag", "npm-publish.yml", "actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1 # v7.1.0",
     "actions/setup-node@v7", 1, "needs the action pinned by SHA"),
    *((f"setup-node {spec}", "npm-publish.yml", "node-version: 24\n          registry-url",
       f"node-version: {spec}\n          registry-url", 1, "a release line or an exact version")
      for spec in ("lts/*", "latest", "current", "node", "'>=24'", "24.x", "''")),
    ("node versions not printed", "npm-publish.yml", PRINT_STEP + "          echo \"npm $v\"\n"
     "          if [ \"$(printf '%s\\n' 11.5.1 \"$v\" | sort -V | head -n1)\" != 11.5.1 ]; then\n"
     "            echo \"::error::npm $v cannot publish through trusted publishing (needs >= 11.5.1)\"\n"
     "            exit 1\n          fi\n      - uses: actions/download-artifact@v8\n        with:\n"
     "          name: npm-native-tarballs", "      - uses: actions/download-artifact@v8\n        with:\n"
     "          name: npm-native-tarballs", 1, "[versions-printed]"),
    ("versions gate removed", "mobile.yml", "      - run: python3 scripts/package-inspect.py versions --tag "
     "\"$GITHUB_REF\" --strict --quiet\n      - uses: actions/download-artifact@v8", "      - uses: "
     "actions/download-artifact@v8", 1, "mobile.yml › ios-release [gate]"),
    ("concurrency removed", "python.yml", "    concurrency:\n      group: publish-${{ github.workflow }}-${{ "
     "github.ref }}\n      cancel-in-progress: false\n    permissions:", "    permissions:", 1,
     "python.yml › release [concurrency]"),
    ("concurrency cancelling", "mobile.yml", "      cancel-in-progress: false\n    permissions:\n      contents: write",
     "      cancel-in-progress: true\n    permissions:\n      contents: write", 1, "mobile.yml › ios-release [concurrency]"),
    ("pnpm install in a publish job", "npm-publish.yml", "      - name: re-inspect the release set about to be uploaded",
     "      - run: pnpm install --frozen-lockfile=false\n      - name: re-inspect the release set about to be "
     "uploaded", 1, "pnpm without --frozen-lockfile"),
    ("npx in a publish job", "npm-publish.yml", "      - name: re-inspect the release set about to be uploaded",
     "      - run: npx some-tool\n      - name: re-inspect the release set about to be uploaded", 1,
     "fetches a package on demand"),
    ("pip install in a publish job", "python.yml", "      - uses: pypa/gh-action-pypi-publish",
     "      - run: pip install twine\n      - uses: pypa/gh-action-pypi-publish", 1, "pip without --require-hashes"),
    ("piped installer in a publish job", "mobile.yml", "      - name: publish + verify the xcframework asset",
     "      - run: curl -sSf https://example.invalid/install.sh | sh\n      - name: publish + verify the xcframework "
     "asset", 1, "pipes a download into a shell"),
    ("cargo publish verifying again", "crates-publish.yml", 'cargo publish -p "$crate" --locked --no-verify',
     'cargo publish -p "$crate" --locked', 1, "[cargo-publish]"),
    ("cargo publish unlocked", "crates-publish.yml", 'cargo publish -p "$crate" --locked --no-verify',
     'cargo publish -p "$crate" --no-verify', 1, "[cargo-publish]"),
    ("pnpm by major line in a publish job", "npm-publish.yml", "      - name: re-inspect the release set about to be "
     "uploaded", "      - uses: pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86 # v6.0.10\n        with:\n"
     "          version: 10\n      - name: re-inspect the release set about to be uploaded", 1, "[floating-pnpm]"),
    ("the Flutter waiver claimed by another job", "python.yml", "      - uses: pypa/gh-action-pypi-publish",
     "      - run: flutter pub get\n      - uses: pypa/gh-action-pypi-publish", 1,
     "FAIL    python.yml › release [unlocked-pub-get]"),
    ("the Flutter job with a third violation", "flutter.yml", "    concurrency:\n      group: publish-${{ "
     "github.workflow }}-${{ github.ref }}\n      cancel-in-progress: false\n", "", 1,
     "FAIL    flutter.yml › publish [concurrency]"),
    ("the Flutter exception gone stale", "flutter.yml", "      - uses: dart-lang/setup-dart@6afc89df92d6eb3834022f7"
     "3cd65adc8cdfcb92d # v1.8.1\n      - uses: subosito/flutter-action@e938fdf56512cc96ef2f93601a5a40bde3801046 # "
     "v2.19.0\n        with:\n          channel: stable\n      - run: flutter pub get\n",
     "      - uses: dart-lang/setup-dart@6afc89df92d6eb3834022f73cd65adc8cdfcb92d # v1.8.1\n        with:\n"
     "          sdk: 3.10.4\n      - uses: subosito/flutter-action@e938fdf56512cc96ef2f93601a5a40bde3801046 # "
     "v2.19.0\n        with:\n          flutter-version: 3.44.2\n      - run: flutter pub get --enforce-lockfile\n",
     1, "stale named exception flutter.yml › publish"),
    ("id-token on an unguarded build job", "ci.yml", "  lint:\n    runs-on: ubuntu-24.04\n",
     "  lint:\n    runs-on: ubuntu-24.04\n    permissions:\n      id-token: write\n", 1, "ci.yml › lint [guard]"),
    ("a release upload in a build job", "mobile.yml", "        run: gradle :qrcodeaiscanner:assembleRelease",
     "        run: gradle :qrcodeaiscanner:assembleRelease && gh release upload v0 x.aar", 1,
     "mobile.yml › android [guard]"),
    ("napi pre-publish that uploads", "npm-publish.yml",
     "pnpm exec napi pre-publish -t npm --no-gh-release --skip-optional-publish",
     "pnpm exec napi pre-publish -t npm --no-gh-release", 1, "npm-publish.yml › pack-native [guard]"),
    ("top-level permissions widened", "deep-checks.yml", "permissions:\n  contents: read\n",
     "permissions:\n  contents: write\n", 1, "top-level permissions must be read-only"),
    ("an anchor the reader refuses", "toolchain-probe.yml", "    runs-on: ubuntu-24.04\n",
     "    runs-on: &runner ubuntu-24.04\n", 2, "anchors, aliases and tags"),
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
    code, out = _run(["--workflows", str(WORKFLOWS)])
    if code != 0:
        failures.append(f"the real workflows: exit {code} (want 0)\n{out}")
    for name, workflow, old, new, want, needle in MUTATIONS:
        with tempfile.TemporaryDirectory(prefix="publish-guards-") as tmp:
            copy = pathlib.Path(tmp) / "workflows"
            shutil.copytree(WORKFLOWS, copy)
            text = (copy / workflow).read_text()
            if old not in text:
                failures.append(f"{name}: anchor not found in {workflow} (update the mutation)")
                continue
            (copy / workflow).write_text(text.replace(old, new, 1))
            code, out = _run(["--workflows", str(copy)])
            if code != want or needle not in out:
                failures.append(f"{name}: exit {code} (want {want}), {needle!r} "
                                f"{'present' if needle in out else 'absent'}\n{out}")
    for failure in failures:
        print(f"SELF-TEST FAILED · {failure}", file=sys.stderr)
    total = 1 + len(MUTATIONS)
    print(f"self-test: {total - len(failures)}/{total} scenarios as expected "
          f"(the real workflows pass; {len(MUTATIONS)} mutated copies each fail)")
    return 1 if failures else 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--workflows", type=pathlib.Path, default=WORKFLOWS, help="the workflows directory")
    parser.add_argument("--self-test", action="store_true", help="run the built-in scenarios, then exit")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    return report(args.workflows)


if __name__ == "__main__":
    sys.exit(main())
