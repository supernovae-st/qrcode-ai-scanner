#!/usr/bin/env python3
"""Stage a portable Flutter package from this candidate's normalized .crate.

Python >= 3.11, stdlib only. Packing uses cargo package --locked --no-verify;
it does not compile. --core-archive accepts an already packed candidate.
--extract safely unpacks the immutable stage in downstream CI jobs.
"""

import argparse
import gzip
import hashlib
import io
import json
import os
import pathlib
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
CORE = "crates/qrcode-ai-scanner"
BINDING = "bindings/flutter"
VENDOR = "rust/vendor/qrcode-ai-scanner"
METADATA = "flutter-bundle"
MAX_ARCHIVE_BYTES = 256 * 1024 * 1024
PUBIGNORE_CORE = b"\n!rust/vendor/qrcode-ai-scanner/.cargo_vcs_info.json\n"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def safe_name(name):
    parts = name.split("/")
    require(bool(name) and all(part not in ("", ".", "..") for part in parts)
            and not re.search(r"[\\:\x00-\x1f\x7f]", name), f"unsafe path: {name!r}")
    return name


def read_archive(data):
    """Read a single tar stream, refusing aliases, links and hidden members."""
    if data[:2] == b"\x1f\x8b":
        with gzip.GzipFile(fileobj=io.BytesIO(data)) as zipped:
            data = zipped.read(MAX_ARCHIVE_BYTES + 1)
    require(len(data) <= MAX_ARCHIVE_BYTES, "archive exceeds size limit")
    files, names, folded = {}, set(), set()
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as archive:
        members = archive.getmembers()
        for member in members:
            name = safe_name(member.name)
            require(name not in names and name.casefold() not in folded, f"duplicate path: {name}")
            names.add(name)
            folded.add(name.casefold())
            require(member.isfile() or member.isdir(), f"non-regular archive member: {name}")
            if member.isfile():
                files[name] = archive.extractfile(member).read()
        if members:
            last = members[-1]
            end = last.offset_data + last.size + (-last.size % 512)
            require(not any(data[end:]), "nonzero data after archive end")
    require(files, "empty archive")
    for name in names:
        require(not any(str(parent) in files for parent in pathlib.PurePosixPath(name).parents),
                f"file/directory collision: {name}")
    return files


def digest(files):
    """SHA-256 of sorted UTF-8 paths and bytes, both length-prefixed."""
    result = hashlib.sha256()
    for name, data in sorted(files.items()):
        for part in (name.encode(), data):
            result.update(len(part).to_bytes(8, "big"))
            result.update(part)
    return result.hexdigest()


def archive_bytes(files):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for name, data in sorted(files.items()):
            member = tarfile.TarInfo(safe_name(name))
            member.size = len(data)
            member.mode = 0o755 if name.endswith(".sh") else 0o644
            archive.addfile(member, io.BytesIO(data))
    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, mode="wb", filename="", mtime=0) as zipped:
        zipped.write(stream.getvalue())
    return compressed.getvalue()


def write_tree(root, files):
    require(not root.exists(), f"output already exists: {root}")
    root.mkdir(parents=True)
    for name, data in sorted(files.items()):
        path = root / safe_name(name)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(0o755 if name.endswith(".sh") else 0o644)


def read_tree(root):
    files = {}
    for path in sorted(root.rglob("*")):
        require(not path.is_symlink(), f"symlink in staged tree: {path}")
        if path.is_file():
            require(path.stat().st_nlink == 1, f"hard link in staged tree: {path}")
            files[safe_name(path.relative_to(root).as_posix())] = path.read_bytes()
        else:
            require(path.is_dir(), f"non-regular staged file: {path}")
    return files


def subfiles(files, prefix):
    return {name[len(prefix) + 1:]: data for name, data in files.items() if name.startswith(prefix + "/")}


def path_dependencies(manifest):
    """Visit normal, dev, build, target, workspace and patch dependency tables."""
    for key, value in manifest.items():
        if key in ("dependencies", "dev-dependencies", "build-dependencies", "replace"):
            for name, dependency in value.items():
                if isinstance(dependency, dict) and "path" in dependency:
                    yield name, dependency["path"]
        elif key == "patch":
            for dependencies in value.values():
                yield from path_dependencies({"dependencies": dependencies})
        elif key in ("workspace", "target"):
            if key == "workspace":
                yield from path_dependencies(value)
            else:
                for target in value.values():
                    yield from path_dependencies(target)


def validate_paths(files):
    for name, data in files.items():
        if pathlib.PurePosixPath(name).name != "Cargo.toml":
            continue
        manifest = tomllib.loads(data.decode())
        for dependency, relative in path_dependencies(manifest):
            require(isinstance(relative, str) and relative and not re.search(r"[\\:\x00-\x1f]", relative)
                    and not relative.startswith("/"), f"invalid path dependency: {name}: {dependency}")
            parts = list(pathlib.PurePosixPath(name).parent.parts)
            for part in relative.split("/"):
                if part == "..":
                    require(parts, f"escaping path dependency: {name}: {dependency}")
                    parts.pop()
                elif part not in ("", "."):
                    parts.append(part)
            target = "/".join([*parts, "Cargo.toml"])
            require(target in files, f"missing path dependency: {name}: {dependency} -> {target}")


def validate_core(files, source, workspace, revision, license_bytes):
    manifest = tomllib.loads(files["Cargo.toml"].decode())
    original = tomllib.loads(source["Cargo.toml"].decode())
    package = manifest["package"]
    for key, value in original["package"].items():
        expected = workspace["package"][key] if isinstance(value, dict) and value.get("workspace") else value
        require(package.get(key) == expected, f"normalized core package mismatch: {key}")
    require(package["name"] == "qrcode-ai-scanner", "wrong core package")
    require(files["LICENSE"] == license_bytes, "core LICENSE mismatch")
    require(files["Cargo.toml.orig"] == source["Cargo.toml"], "core source manifest mismatch")
    vcs = json.loads(files[".cargo_vcs_info.json"])
    require(vcs.get("git", {}).get("sha1") == revision and not vcs["git"].get("dirty")
            and vcs.get("path_in_vcs") == CORE, "core candidate/source revision mismatch")
    generated = {"Cargo.toml", "Cargo.toml.orig", "Cargo.lock", ".cargo_vcs_info.json"}
    require(set(files) - generated == set(source) - {"Cargo.toml"}, "core source file set mismatch")
    for name, data in source.items():
        if name != "Cargo.toml":
            require(files[name] == data, f"core source bytes mismatch: {name}")
    for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
        expected = {}
        for name, dependency in original.get(kind, {}).items():
            dependency = {"version": dependency} if isinstance(dependency, str) else dict(dependency)
            if dependency.pop("workspace", False):
                inherited = workspace["dependencies"][name]
                inherited = {"version": inherited} if isinstance(inherited, str) else dict(inherited)
                dependency = inherited | dependency
            expected[name] = dependency
        require(manifest.get(kind, {}) == expected, f"normalized core {kind} mismatch")
    require(manifest.get("features", {}) == original.get("features", {}), "core features mismatch")
    require("workspace" not in manifest and "workspace" not in package, "core archive still inherits a workspace")
    for kind in ("lib", "bin", "test", "bench", "example"):
        targets = manifest.get(kind, [])
        targets = [targets] if isinstance(targets, dict) else targets
        for target in targets:
            if "path" in target:
                path = safe_name(target["path"])
                require(path in files, f"missing core target: {path}")
    validate_paths(files)


def bundle(core_archive, snapshot, revision):
    workspace = tomllib.loads(snapshot["Cargo.toml"].decode())["workspace"]
    version = workspace["package"]["version"]
    packed = read_archive(core_archive)
    prefix = f"qrcode-ai-scanner-{version}"
    require(all(name.startswith(prefix + "/") for name in packed), "core archive name/version mismatch")
    core = subfiles(packed, prefix)
    source = subfiles(snapshot, CORE)
    validate_core(core, source, workspace, revision, snapshot["LICENSE"])
    files = subfiles(snapshot, BINDING)
    require(files["LICENSE"] == snapshot["LICENSE"], "binding LICENSE mismatch")
    text = files["rust/Cargo.toml"].decode()
    binding = tomllib.loads(text)
    require(binding["package"]["version"] == version, "binding/core version mismatch")
    require(re.search(r"^version:\s*" + re.escape(version) + r"\s*$", files["pubspec.yaml"].decode(), re.M),
            "pubspec/core version mismatch")
    dependency = binding["dependencies"]["scanner-core"]
    require(dependency.get("package") == "qrcode-ai-scanner"
            and dependency.get("path") == "../../../crates/qrcode-ai-scanner", "unexpected scanner-core dependency")
    require(METADATA not in binding["package"].get("metadata", {}), "binding already staged")
    rewritten, count = re.subn(r'(scanner-core\s*=\s*\{[^\n]*?\bpath\s*=\s*)"[^"]+"',
                              r'\1"vendor/qrcode-ai-scanner"', text)
    require(count == 1, "cannot rewrite exactly one scanner-core.path")
    metadata = {"candidate": revision, "core-version": version, "core-content-sha256": digest(core),
                "core-source-sha256": digest(source), "core-archive-sha256": hashlib.sha256(core_archive).hexdigest()}
    rewritten += f"\n[package.metadata.{METADATA}]\n"
    rewritten += "".join(f"{key} = {json.dumps(value)}\n" for key, value in metadata.items())
    files["rust/Cargo.toml"] = rewritten.encode()
    require(".pubignore" not in files, "review existing .pubignore before staging")
    files[".pubignore"] = files.get(".gitignore", b"") + PUBIGNORE_CORE
    files.update({f"{VENDOR}/{name}": data for name, data in core.items()})
    validate_paths(files)
    require(files["rust/Cargo.lock"] == snapshot[f"{BINDING}/rust/Cargo.lock"], "binding lockfile changed")
    return files, metadata | {"package-content-sha256": digest(files)}


def candidate_snapshot(root, revision):
    return read_archive(subprocess.check_output(
        ["git", "archive", "--format=tar", revision, "Cargo.toml", "LICENSE", CORE, BINDING], cwd=root))


def verify_staged(files, snapshot, revision):
    """Check staged source identity and the digest that invalidates cargokit's cache."""
    core = subfiles(files, VENDOR)
    workspace = tomllib.loads(snapshot["Cargo.toml"].decode())["workspace"]
    validate_core(core, subfiles(snapshot, CORE), workspace, revision, snapshot["LICENSE"])
    manifest = tomllib.loads(files["rust/Cargo.toml"].decode())
    metadata = manifest["package"]["metadata"][METADATA]
    require(metadata["candidate"] == revision, "staged candidate mismatch")
    require(metadata["core-content-sha256"] == digest(core), "bundled core content digest mismatch")
    require(metadata["core-source-sha256"] == digest(subfiles(snapshot, CORE)), "bundled core source digest mismatch")
    require(metadata["core-version"] == workspace["package"]["version"], "bundled core version mismatch")
    require(re.fullmatch(r"[0-9a-f]{64}", metadata["core-archive-sha256"]), "invalid core archive digest")
    binding = subfiles(snapshot, BINDING)
    require(files.get(".pubignore") == binding.get(".gitignore", b"") + PUBIGNORE_CORE, "staged pubignore mismatch")
    canonical = tomllib.loads(binding["rust/Cargo.toml"].decode())
    manifest["package"]["metadata"].pop(METADATA)
    if not manifest["package"]["metadata"]:
        manifest["package"].pop("metadata")
    require(manifest["dependencies"]["scanner-core"]["path"] == "vendor/qrcode-ai-scanner", "wrong bundled core path")
    manifest["dependencies"]["scanner-core"]["path"] = canonical["dependencies"]["scanner-core"]["path"]
    require(manifest == canonical, "staged binding manifest changed beyond path and bundle metadata")
    for name, data in binding.items():
        if name != "rust/Cargo.toml":
            require(files.get(name) == data, f"staged binding source mismatch: {name}")
    validate_paths(files)
    return metadata


def fixture(version="0.9.0"):
    revision = "a" * 40
    original = b'[package]\nname = "qrcode-ai-scanner"\nversion.workspace = true\nlicense.workspace = true\n'
    workspace = b'[workspace.package]\nversion = "0.9.0"\nlicense = "AGPL-3.0-or-later"\n'
    manifest = b'[package]\nname = "qrcode-ai-scanner"\nversion = "0.9.0"\nlicense = "AGPL-3.0-or-later"\n'
    core = {"Cargo.toml": manifest, "Cargo.toml.orig": original, "LICENSE": b"license\n",
            "src/lib.rs": b"pub fn value() -> u32 { 1 }\n", "Cargo.lock": b"version = 4\n",
            ".cargo_vcs_info.json": json.dumps({"git": {"sha1": revision}, "path_in_vcs": CORE}).encode()}
    binding = b'[package]\nname = "qrcode_ai_scanner"\nversion = "0.9.0"\n[dependencies]\nscanner-core = { package = "qrcode-ai-scanner", path = "../../../crates/qrcode-ai-scanner", features = ["serde"] }\n'
    snapshot = {"Cargo.toml": workspace, "LICENSE": b"license\n", f"{CORE}/Cargo.toml": original,
                f"{CORE}/LICENSE": b"license\n", f"{CORE}/src/lib.rs": core["src/lib.rs"],
                f"{BINDING}/LICENSE": b"license\n", f"{BINDING}/pubspec.yaml": b"version: 0.9.0\n",
                f"{BINDING}/rust/Cargo.toml": binding, f"{BINDING}/rust/Cargo.lock": b"version = 4\n"}
    core = {name: data.replace(b"0.9.0", version.encode()) for name, data in core.items()}
    snapshot = {name: data.replace(b"0.9.0", version.encode()) for name, data in snapshot.items()}
    return core, snapshot, revision


def self_test():
    class StagingTests(unittest.TestCase):
        def setUp(self):
            self.core, self.snapshot, self.revision = fixture()

        def pack(self, core=None):
            return archive_bytes({f"qrcode-ai-scanner-0.9.0/{name}": data
                                  for name, data in (core or self.core).items()})

        def test_identity_and_determinism(self):
            first, receipt = bundle(self.pack(), self.snapshot, self.revision)
            second, repeated = bundle(self.pack(), self.snapshot, self.revision)
            self.assertEqual(first, second)
            self.assertEqual(receipt, repeated)
            self.assertEqual(archive_bytes(first), archive_bytes(second))
            self.assertEqual(archive_bytes(first)[4:10], b"\x00\x00\x00\x00\x02\xff")
            self.assertEqual(read_archive(archive_bytes(first)), first)
            self.assertEqual(first["rust/Cargo.lock"], self.snapshot[f"{BINDING}/rust/Cargo.lock"])
            verify_staged(first, self.snapshot, self.revision)

        def test_core_only_change_invalidates_root_manifest(self):
            before, _ = bundle(self.pack(), self.snapshot, self.revision)
            self.core["src/lib.rs"] = b"pub fn value() -> u32 { 2 }\n"
            self.snapshot[f"{CORE}/src/lib.rs"] = self.core["src/lib.rs"]
            after, _ = bundle(self.pack(), self.snapshot, self.revision)
            self.assertNotEqual(before["rust/Cargo.toml"], after["rust/Cargo.toml"])
            self.assertNotEqual(digest(before), digest(after))

        def test_identity_mismatches(self):
            for name, value in (("src/lib.rs", b"old registry core"), ("LICENSE", b"other"),
                                ("Cargo.toml.orig", b"other"), ("Cargo.toml", self.core["Cargo.toml"].replace(b"0.9.0", b"0.8.0")),
                                (".cargo_vcs_info.json", b'{"git":{"sha1":"old"}}')):
                with self.subTest(name=name), self.assertRaises((ValueError, KeyError)):
                    bundle(self.pack(self.core | {name: value}), self.snapshot, self.revision)
            for name, value in ((f"{BINDING}/pubspec.yaml", b"version: 0.8.0\n"),
                                (f"{BINDING}/LICENSE", b"wrong")):
                with self.subTest(name=name), self.assertRaises(ValueError):
                    bundle(self.pack(), self.snapshot | {name: value}, self.revision)

        def test_malicious_archives(self):
            for name, kind in (("../escape", tarfile.REGTYPE), ("/absolute", tarfile.REGTYPE),
                               ("a/../../escape", tarfile.REGTYPE), ("a\\escape", tarfile.REGTYPE),
                               ("C:/escape", tarfile.REGTYPE), ("a//b", tarfile.REGTYPE),
                               ("a/./b", tarfile.REGTYPE), ("link", tarfile.SYMTYPE),
                               ("hard", tarfile.LNKTYPE), ("fifo", tarfile.FIFOTYPE)):
                stream = io.BytesIO()
                with tarfile.open(fileobj=stream, mode="w") as archive:
                    member = tarfile.TarInfo(name)
                    member.type, member.linkname = kind, "../outside"
                    archive.addfile(member, io.BytesIO())
                with self.subTest(name=name), self.assertRaises(ValueError):
                    read_archive(stream.getvalue())
            for names in (("a", "a"), ("A", "a"), ("a", "a/b")):
                stream = io.BytesIO()
                with tarfile.open(fileobj=stream, mode="w") as archive:
                    for name in names:
                        archive.addfile(tarfile.TarInfo(name), io.BytesIO())
                with self.subTest(names=names), self.assertRaises(ValueError):
                    read_archive(stream.getvalue())
            with self.assertRaises(ValueError):
                read_archive(gzip.decompress(self.pack()) + b"hidden")

        def test_normalized_dependencies(self):
            self.snapshot["Cargo.toml"] += b'\n[workspace.dependencies]\nserde = "1"\n'
            original = self.snapshot[f"{CORE}/Cargo.toml"] + b'\n[dependencies]\nserde.workspace = true\n'
            self.snapshot[f"{CORE}/Cargo.toml"] = original
            self.core["Cargo.toml.orig"] = original
            self.core["Cargo.toml"] += b'\n[dependencies.serde]\nversion = "1"\n'
            bundle(self.pack(), self.snapshot, self.revision)
            self.core["Cargo.toml"] = self.core["Cargo.toml"].replace(b'version = "1"', b'version = "2"')
            with self.assertRaisesRegex(ValueError, "dependencies mismatch"):
                bundle(self.pack(), self.snapshot, self.revision)

        def test_recursive_path_dependencies(self):
            for section in ("dependencies", "dev-dependencies", "build-dependencies",
                            "target.'cfg(unix)'.dependencies", "patch.crates-io", "workspace.dependencies", "replace"):
                with self.subTest(section=section), self.assertRaisesRegex(ValueError, "escaping"):
                    validate_paths({"nested/Cargo.toml": f'[{section}]\nbad = {{ path = "../../outside" }}\n'.encode()})
            with self.assertRaisesRegex(ValueError, "missing"):
                validate_paths({"Cargo.toml": b'[dependencies]\nbad = { path = "absent" }\n'})
            validate_paths({"Cargo.toml": b'[dependencies]\ngood = { path = "nested" }\n', "nested/Cargo.toml": b""})

        def test_tampered_stage_and_links(self):
            staged, _ = bundle(self.pack(), self.snapshot, self.revision)
            for name, data in ((f"{VENDOR}/src/lib.rs", b"changed"), ("rust/Cargo.lock", b"changed"),
                               ("rust/Cargo.toml", staged["rust/Cargo.toml"].replace(b"vendor/qrcode", b"../qrcode"))):
                with self.subTest(name=name), self.assertRaises(ValueError):
                    verify_staged(staged | {name: data}, self.snapshot, self.revision)
            with tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary) / "stage"
                write_tree(root, staged)
                self.assertEqual(read_tree(root), staged)
                (root / "link").symlink_to(root / "LICENSE")
                with self.assertRaisesRegex(ValueError, "symlink"):
                    read_tree(root)
                (root / "link").unlink()
                os.link(root / "LICENSE", root / "hard")
                with self.assertRaisesRegex(ValueError, "hard link"):
                    read_tree(root)

    return 0 if unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(StagingTests)).wasSuccessful() else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--archive", type=pathlib.Path, help="write deterministic staged .tar.gz")
    parser.add_argument("--receipt", type=pathlib.Path)
    parser.add_argument("--core-archive", type=pathlib.Path)
    parser.add_argument("--cargo-wrapper", help="invoke Cargo as WRAPPER cargo ...")
    parser.add_argument("--extract", type=pathlib.Path, help="safely extract a previously staged archive; no Cargo")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if not args.output:
        parser.error("--output is required")
    if args.extract:
        write_tree(args.output, read_archive(args.extract.read_bytes()))
        return 0
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    subprocess.run(["git", "diff", "--exit-code", "HEAD", "--", CORE, BINDING, "Cargo.toml", "Cargo.lock", "LICENSE"],
                   cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
    snapshot = candidate_snapshot(ROOT, revision)
    cargo = [args.cargo_wrapper, "cargo"] if args.cargo_wrapper else ["cargo"]
    archive = args.core_archive
    if archive is None:
        subprocess.run([*cargo, "package", "--locked", "--no-verify", "-p", "qrcode-ai-scanner"], cwd=ROOT, check=True)
        metadata = json.loads(subprocess.check_output([*cargo, "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT))
        version = tomllib.loads(snapshot["Cargo.toml"].decode())["workspace"]["package"]["version"]
        archive = pathlib.Path(metadata["target_directory"]) / "package" / f"qrcode-ai-scanner-{version}.crate"
    files, receipt = bundle(archive.read_bytes(), snapshot, revision)
    write_tree(args.output, files)
    if args.archive:
        packed = archive_bytes(files)
        args.archive.write_bytes(packed)
        receipt["package-archive-sha256"] = hashlib.sha256(packed).hexdigest()
    if args.receipt:
        args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, OSError, tarfile.TarError, subprocess.CalledProcessError) as error:
        sys.exit(f"stage-flutter-package: {error}")
