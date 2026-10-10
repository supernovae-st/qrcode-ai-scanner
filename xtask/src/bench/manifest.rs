//! Build manifests. Every measured artifact carries the facts of its build:
//! the compiler that ran (`rustc -vV`), the compiler commits embedded in
//! the binary, the effective release profile with its `--config` overrides,
//! the features asked for, the lockfile, the source it came from with its
//! dirty flag — and what the build itself reported. `bench stamp` runs the
//! build: for cargo it reads the `--message-format=json` stream (how the
//! artifact's unit, the scanner and rxing compiled), for a WASM package it
//! records the binaryen and the script's recipe; both keep the codegen
//! environment. The manifest lands next to the artifact
//! (`<file>.build.json`, or `build.json` inside a package directory);
//! `bench run` reads it back, checks that it describes the file it
//! measures, and refuses a gated pair whose compilers, profiles, builds or
//! recipes differ.
//!
//! A directory a harness step generated or extracted carries a
//! [`SOURCE_MARK`] with the files it wrote and their hash, so a later stamp
//! can tell whether they changed since.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::external;

/// Manifest format identifier.
pub(crate) const FORMAT: &str = "qrscan-bench-build/v2";
/// The source mark a harness step leaves in a directory it wrote.
pub(crate) const SOURCE_MARK: &str = ".bench-source.json";
/// The manifest of a package directory.
pub(crate) const PACKAGE_MANIFEST: &str = "build.json";

/// Cargo's built-in `release` profile — the base every profile table and
/// `--config` override lands on (values as normalized TOML text). `strip`
/// defaults to `"debuginfo"` since Rust 1.77 when no debug info is asked.
const RELEASE_DEFAULTS: [(&str, &str); 10] = [
    ("codegen-units", "16"),
    ("debug", "false"),
    ("debug-assertions", "false"),
    ("incremental", "false"),
    ("lto", "false"),
    ("opt-level", "3"),
    ("overflow-checks", "false"),
    ("panic", "\"unwind\""),
    ("rpath", "false"),
    ("strip", "\"debuginfo\""),
];

/// `rustc -vV` of the toolchain that ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Rustc {
    pub(crate) release: String,
    pub(crate) commit_hash: String,
    pub(crate) host: String,
    pub(crate) llvm: String,
}

pub(crate) fn parse_rustc_vv(text: &str) -> Option<Rustc> {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_owned())
    };
    Some(Rustc {
        release: field("release:")?,
        commit_hash: field("commit-hash:")?,
        host: field("host:").unwrap_or_default(),
        llvm: field("LLVM version:").unwrap_or_default(),
    })
}

/// `rustc -vV` from `PATH`, under the caller's toolchain environment.
pub(crate) fn rustc_now() -> Option<Rustc> {
    let out = Command::new("rustc").arg("-vV").output().ok()?;
    out.status
        .success()
        .then(|| parse_rustc_vv(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

/// Compiler commits a binary embeds — `/rustc/<40 hex>/`, the prefix of
/// std's panic locations, which survives `strip` because it is data, not
/// debug information. Sorted, distinct.
pub(crate) fn embedded_rustc_commits(bytes: &[u8]) -> Vec<String> {
    const NEEDLE: &[u8] = b"/rustc/";
    let mut found = BTreeSet::new();
    let mut at = 0;
    while let Some(pos) = bytes
        .get(at..)
        .and_then(|rest| rest.windows(NEEDLE.len()).position(|w| w == NEEDLE))
    {
        let start = at + pos + NEEDLE.len();
        if let Some(hash) = bytes.get(start..start + 40)
            && hash
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            && bytes.get(start + 40) == Some(&b'/')
        {
            found.insert(String::from_utf8_lossy(hash).into_owned());
        }
        at = start;
    }
    found.into_iter().collect()
}

/// Where an artifact's source came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceId {
    /// `git` · `archive` · `generated` · `crate` · `unknown`.
    pub(crate) kind: String,
    pub(crate) origin: String,
    /// A commit, or the sha256 of a published archive.
    pub(crate) sha: String,
    /// `clean` · `dirty` · `unknown`.
    pub(crate) tree: String,
}

/// What a harness step records about a directory it wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceMark {
    pub(crate) kind: String,
    pub(crate) origin: String,
    pub(crate) sha: String,
    pub(crate) tree: String,
    /// The files the step wrote, relative to the directory.
    pub(crate) files: Vec<String>,
    /// sha256 over the sorted `path<TAB>sha256` lines of `files`.
    pub(crate) files_sha256: String,
}

/// sha256 over the sorted `path<TAB>sha256` lines of `files` in `dir`.
pub(crate) fn files_sha256(dir: &Path, files: &[String]) -> Result<String, String> {
    let mut sorted = files.to_vec();
    sorted.sort();
    let mut lines = String::new();
    for name in &sorted {
        let bytes = std::fs::read(dir.join(name)).map_err(|e| format!("{name}: {e}"))?;
        lines.push_str(name);
        lines.push('\t');
        lines.push_str(&external::sha256_bytes(&bytes));
        lines.push('\n');
    }
    Ok(external::sha256_bytes(lines.as_bytes()))
}

/// Leave a [`SOURCE_MARK`] in `dir` for `files` (already written).
pub(crate) fn mark_source(
    dir: &Path,
    kind: &str,
    origin: &str,
    (sha, tree): (&str, &str),
    files: &[String],
) -> Result<(), String> {
    let mark = SourceMark {
        kind: kind.to_owned(),
        origin: origin.to_owned(),
        sha: sha.to_owned(),
        tree: tree.to_owned(),
        files: files.to_vec(),
        files_sha256: files_sha256(dir, files)?,
    };
    let mut text = serde_json::to_string_pretty(&mark).map_err(|e| e.to_string())?;
    text.push('\n');
    std::fs::write(dir.join(SOURCE_MARK), text).map_err(|e| format!("{SOURCE_MARK}: {e}"))
}

/// The identity of `dir`: its source mark (dirty once a marked file
/// changed), else the git work tree rooted exactly there, else unknown.
pub(crate) fn source_of(dir: &Path) -> SourceId {
    if let Ok(text) = std::fs::read_to_string(dir.join(SOURCE_MARK))
        && let Ok(mark) = serde_json::from_str::<SourceMark>(&text)
    {
        let unchanged = files_sha256(dir, &mark.files).is_ok_and(|h| h == mark.files_sha256);
        return SourceId {
            kind: mark.kind,
            origin: mark.origin,
            sha: mark.sha,
            tree: if unchanged {
                mark.tree
            } else {
                String::from("dirty")
            },
        };
    }
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    let canonical = |p: &Path| std::fs::canonicalize(p).ok();
    let top = git(&["rev-parse", "--show-toplevel"]).map(PathBuf::from);
    if top.as_deref().and_then(canonical).is_some()
        && top.as_deref().and_then(canonical) == canonical(dir)
    {
        let source = super::env::source(dir);
        return SourceId {
            kind: String::from("git"),
            origin: String::from("git work tree"),
            sha: source.git_sha,
            tree: source.tree,
        };
    }
    SourceId {
        kind: String::from("unknown"),
        origin: String::new(),
        sha: String::from("unknown"),
        tree: String::from("unknown"),
    }
}

/// A lockfile: its hash and its `name version` package list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Lock {
    pub(crate) sha256: String,
    pub(crate) packages: Vec<String>,
}

pub(crate) fn parse_lock(text: &str) -> Result<Lock, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| format!("Cargo.lock: {e}"))?;
    let mut packages: Vec<String> = table
        .get("package")
        .and_then(toml::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|p| {
                    Some(format!(
                        "{} {}",
                        p.get("name")?.as_str()?,
                        p.get("version")?.as_str()?
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    packages.sort();
    packages.dedup();
    Ok(Lock {
        sha256: external::sha256_bytes(text.as_bytes()),
        packages,
    })
}

/// Lock delta of a pair: shared names at another version, and how many
/// packages only one side has (dev-dependencies and other members land
/// there; they are not linked into a release binary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LockDelta {
    pub(crate) same: usize,
    pub(crate) version_differs: Vec<String>,
    pub(crate) only_base: usize,
    pub(crate) only_candidate: usize,
}

pub(crate) fn lock_delta(base: &Lock, candidate: &Lock) -> LockDelta {
    let index = |lock: &Lock| -> BTreeMap<String, BTreeSet<String>> {
        let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for package in &lock.packages {
            if let Some((name, version)) = package.split_once(' ') {
                map.entry(name.to_owned())
                    .or_default()
                    .insert(version.to_owned());
            }
        }
        map
    };
    let (a, b) = (index(base), index(candidate));
    let mut delta = LockDelta {
        same: 0,
        version_differs: Vec::new(),
        only_base: 0,
        only_candidate: 0,
    };
    for (name, versions) in &a {
        match b.get(name) {
            Some(other) if other == versions => delta.same += 1,
            Some(other) => delta.version_differs.push(format!(
                "{name} {} -> {}",
                versions.iter().cloned().collect::<Vec<_>>().join(","),
                other.iter().cloned().collect::<Vec<_>>().join(",")
            )),
            None => delta.only_base += 1,
        }
    }
    delta.only_candidate = b.keys().filter(|name| !a.contains_key(*name)).count();
    delta
}

/// A TOML value as normalized text, with cargo's equivalent spellings of
/// `debug`, `strip` and `lto` folded together.
fn normalized(key: &str, value: &toml::Value) -> String {
    use toml::Value::{Boolean, Integer, String as Text};
    let leaf = key.rsplit('.').next().unwrap_or(key);
    match (leaf, value) {
        ("debug", Boolean(false) | Integer(0)) => String::from("false"),
        ("debug", Text(s)) if s == "none" => String::from("false"),
        ("debug", Boolean(true) | Integer(2)) => String::from("true"),
        ("debug", Text(s)) if s == "full" => String::from("true"),
        ("debug", Integer(1)) => String::from("\"limited\""),
        ("strip", Boolean(false)) => String::from("\"none\""),
        ("strip", Boolean(true)) => String::from("\"symbols\""),
        ("lto", Boolean(true)) => String::from("\"fat\""),
        _ => value.to_string(),
    }
}

/// Flatten one profile table: scalars by name, `package.<name>.<key>` for
/// package overrides, `build-override.<key>` for build scripts.
fn flatten_profile(table: &toml::Table, out: &mut BTreeMap<String, String>) {
    for (key, value) in table {
        match (key.as_str(), value) {
            ("package", toml::Value::Table(packages)) => {
                for (name, settings) in packages {
                    if let toml::Value::Table(settings) = settings {
                        for (k, v) in settings {
                            let key = format!("package.{name}.{k}");
                            out.insert(key.clone(), normalized(&key, v));
                        }
                    }
                }
            }
            ("build-override", toml::Value::Table(settings)) => {
                for (k, v) in settings {
                    let key = format!("build-override.{k}");
                    out.insert(key.clone(), normalized(&key, v));
                }
            }
            (_, toml::Value::Table(_)) => {}
            _ => {
                out.insert(key.clone(), normalized(key, value));
            }
        }
    }
}

/// The effective `release` profile of a build: cargo's defaults, then the
/// source manifest's `[profile.release]` tables, then the `--config`
/// overrides (`profile.release.<key>=<value>`). A package override equal to
/// the profile-wide value says nothing and is dropped, so a published
/// crate built with defaults and a workspace profile overridden back to
/// the defaults compare equal. Returns the profile and the overrides that
/// are not profile settings.
pub(crate) fn effective_profile(
    manifest_text: Option<&str>,
    overrides: &[String],
) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let mut profile: BTreeMap<String, String> = RELEASE_DEFAULTS
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    if let Some(text) = manifest_text {
        let table: toml::Table = toml::from_str(text).map_err(|e| format!("Cargo.toml: {e}"))?;
        if let Some(release) = table
            .get("profile")
            .and_then(|p| p.get("release"))
            .and_then(toml::Value::as_table)
        {
            flatten_profile(release, &mut profile);
        }
    }
    let mut other = Vec::new();
    for item in overrides {
        let (path, value) = item
            .split_once('=')
            .ok_or_else(|| format!("--config {item:?}: expected key=value"))?;
        let Some(key) = path.trim().strip_prefix("profile.release.") else {
            other.push(item.clone());
            continue;
        };
        let parsed: toml::Table = toml::from_str(&format!("v = {}", value.trim()))
            .map_err(|e| format!("--config {item:?}: {e}"))?;
        let value = parsed.get("v").ok_or("--config: no value")?;
        profile.insert(key.to_owned(), normalized(key, value));
    }
    let redundant: Vec<String> = profile
        .iter()
        .filter_map(|(key, value)| {
            let rest = key.strip_prefix("package.")?;
            let (_, leaf) = rest.rsplit_once('.')?;
            (profile.get(leaf) == Some(value)).then(|| key.clone())
        })
        .collect();
    for key in redundant {
        profile.remove(&key);
    }
    Ok((profile, other))
}

/// The artifact a manifest describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactId {
    pub(crate) name: String,
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
}

/// One build manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) format: String,
    pub(crate) artifact: ArtifactId,
    pub(crate) rustc: Option<Rustc>,
    pub(crate) embedded_rustc_commits: Vec<String>,
    /// The effective release profile ([`effective_profile`]).
    pub(crate) profile: BTreeMap<String, String>,
    /// Every `--config` value the build command passed, as passed.
    pub(crate) config_overrides: Vec<String>,
    /// `--config` values that are not release-profile settings.
    pub(crate) other_config: Vec<String>,
    /// The features the build command asked for (`default` when none).
    pub(crate) features: Vec<String>,
    pub(crate) lock: Option<Lock>,
    pub(crate) source: SourceId,
    pub(crate) stamped_utc: String,
    /// A cargo build's observations ([`stamp_build`]).
    #[serde(default)]
    pub(crate) build: Option<Build>,
    /// A WASM package's recipe ([`stamp_wasm`]).
    #[serde(default)]
    pub(crate) wasm_build: Option<WasmBuild>,
}

/// Build the manifest of `artifact`, built from the cargo root `source`
/// with these `--config` overrides and features.
pub(crate) fn stamp(
    artifact: &Path,
    source: &Path,
    overrides: &[String],
    features: &[String],
) -> Result<Manifest, String> {
    let bytes = std::fs::read(artifact).map_err(|e| format!("{}: {e}", artifact.display()))?;
    let name = artifact
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("the artifact has no file name")?;
    let manifest_text = std::fs::read_to_string(source.join("Cargo.toml")).ok();
    if manifest_text.is_none() {
        return Err(format!("{}: no Cargo.toml", source.display()));
    }
    let (profile, other_config) = effective_profile(manifest_text.as_deref(), overrides)?;
    let lock = match std::fs::read_to_string(source.join("Cargo.lock")) {
        Ok(text) => Some(parse_lock(&text)?),
        Err(_) => None,
    };
    let features = if features.is_empty() {
        vec![String::from("default")]
    } else {
        features.to_vec()
    };
    Ok(Manifest {
        format: FORMAT.to_owned(),
        artifact: ArtifactId {
            name,
            sha256: external::sha256_bytes(&bytes),
            bytes: bytes.len() as u64,
        },
        rustc: rustc_now(),
        embedded_rustc_commits: embedded_rustc_commits(&bytes),
        profile,
        config_overrides: overrides.to_vec(),
        other_config,
        features,
        lock,
        source: source_of(source),
        stamped_utc: super::env::utc(super::gate::unix_now()),
        build: None,
        wasm_build: None,
    })
}

/// One compilation unit as cargo reported it for the stamped build (a
/// `compiler-artifact` message): the profile it was compiled with and the
/// features it was compiled with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Unit {
    pub(crate) package: String,
    pub(crate) version: String,
    pub(crate) opt_level: String,
    pub(crate) debuginfo: String,
    pub(crate) debug_assertions: bool,
    pub(crate) overflow_checks: bool,
    pub(crate) features: Vec<String>,
}

/// The roles whose units a build manifest keeps: the unit that produced
/// the artifact, the scanner, and rxing (its overflow-checks override).
pub(crate) const ROLES: [&str; 3] = ["artifact", "qrcode-ai-scanner", "rxing"];

/// What the stamp step observed of the build it ran: the command, the
/// units of [`ROLES`], and the codegen-affecting environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Build {
    /// The command as run, every path cut to its last component.
    pub(crate) command: Vec<String>,
    pub(crate) units: BTreeMap<String, Unit>,
    /// `*RUSTFLAGS*`, `CARGO_PROFILE_*`, `RUSTC` and the rustc wrappers,
    /// name → sha256 of the value (a value can carry a path).
    pub(crate) env: BTreeMap<String, String>,
}

/// A WASM package's build beyond its module: the binaryen that optimized
/// it, with the flags and `RUSTFLAGS` its build script passes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WasmBuild {
    pub(crate) command: Vec<String>,
    /// `wasm-opt --version` of the binaryen on the build's `PATH`.
    pub(crate) wasm_opt: Option<String>,
    pub(crate) wasm_opt_flags: Vec<String>,
    pub(crate) rustflags: Option<String>,
    pub(crate) script_sha256: Option<String>,
    pub(crate) env: BTreeMap<String, String>,
}

/// The command with every argument that holds a path cut to its last
/// component: a manifest reaches receipts, which carry no absolute path.
pub(crate) fn redacted(command: &[String]) -> Vec<String> {
    command
        .iter()
        .map(|arg| match arg.rsplit_once('/') {
            Some((_, tail)) => format!("…/{tail}"),
            None => arg.clone(),
        })
        .collect()
}

/// The values of a cargo command line's `--config` arguments.
pub(crate) fn config_args(command: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = command.iter();
    while let Some(arg) = args.next() {
        if arg == "--config" {
            out.extend(args.next().cloned());
        } else if let Some(value) = arg.strip_prefix("--config=") {
            out.push(value.to_owned());
        }
    }
    out
}

/// The features a cargo command line asks for: `--features`/`-F` values,
/// `--no-default-features`, `--all-features` — the operator's choice, which
/// both sides of a pair must share; `default` when none.
pub(crate) fn requested_features(command: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = command.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--features" | "-F" => {
                if let Some(list) = args.next() {
                    out.extend(
                        list.split([',', ' '])
                            .filter(|f| !f.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
            "--no-default-features" | "--all-features" => out.push(arg.clone()),
            other => {
                if let Some(list) = other.strip_prefix("--features=") {
                    out.extend(
                        list.split([',', ' '])
                            .filter(|f| !f.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
        }
    }
    if out.is_empty() {
        out.push(String::from("default"));
    }
    out.sort();
    out.dedup();
    out
}

/// The codegen-affecting variables of `vars`, name → sha256 of the value.
pub(crate) fn build_env(
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> BTreeMap<String, String> {
    vars.filter_map(|(name, value)| {
        let name = name.into_string().ok()?;
        let kept = name.contains("RUSTFLAGS")
            || name.starts_with("CARGO_PROFILE_")
            || matches!(
                name.as_str(),
                "RUSTC" | "RUSTC_WRAPPER" | "RUSTC_WORKSPACE_WRAPPER"
            );
        kept.then(|| (name, external::sha256_bytes(value.as_encoded_bytes())))
    })
    .collect()
}

/// Package name and version of a cargo package id: `registry+…#name@1.0.0`,
/// `path+file:///…/name#1.0.0` (the name is the last path segment when the
/// fragment omits it), or the older `name 1.0.0 (source)`.
pub(crate) fn package_of(id: &str) -> Option<(String, String)> {
    if let Some((url, fragment)) = id.split_once('#') {
        return match fragment.split_once('@') {
            Some((name, version)) => Some((name.to_owned(), version.to_owned())),
            None => Some((url.rsplit('/').next()?.to_owned(), fragment.to_owned())),
        };
    }
    let mut words = id.split_whitespace();
    Some((words.next()?.to_owned(), words.next()?.to_owned()))
}

/// The units of [`ROLES`] in a `--message-format=json` stream: the one
/// whose files include `artifact` (canonical), the scanner's and rxing's.
pub(crate) fn units_of(stream: &str, artifact: &Path) -> Result<BTreeMap<String, Unit>, String> {
    let artifact =
        std::fs::canonicalize(artifact).map_err(|e| format!("the artifact: {}", e.kind()))?;
    let mut units = BTreeMap::new();
    for line in stream.lines().filter(|l| l.starts_with('{')) {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        let Some((package, version)) = message["package_id"].as_str().and_then(package_of) else {
            continue;
        };
        let profile = &message["profile"];
        let unit = Unit {
            package: package.clone(),
            version,
            opt_level: profile["opt_level"].as_str().unwrap_or("?").to_owned(),
            debuginfo: profile["debuginfo"].to_string(),
            debug_assertions: profile["debug_assertions"].as_bool().unwrap_or(true),
            overflow_checks: profile["overflow_checks"].as_bool().unwrap_or(true),
            features: message["features"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|f| f.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        };
        let produced = message["filenames"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(std::iter::once(&message["executable"]))
            .filter_map(|f| f.as_str())
            .any(|f| std::fs::canonicalize(f).is_ok_and(|f| f == artifact));
        if produced {
            units.insert(String::from("artifact"), unit);
        } else if ROLES.contains(&package.as_str()) && message["target"]["kind"][0] == "lib" {
            units.insert(package, unit);
        }
    }
    if !units.contains_key("artifact") {
        return Err(String::from(
            "the build reported no unit producing the artifact",
        ));
    }
    Ok(units)
}

/// Run `command` (a cargo build) and stamp the artifact it produced: the
/// profile, overrides and features come from the build itself — its
/// arguments and cargo's JSON messages — never from a declaration.
pub(crate) fn stamp_build(
    artifact: &Path,
    source: &Path,
    command: &[String],
) -> Result<Manifest, String> {
    let (program, args) = command
        .split_first()
        .ok_or("stamp runs the build: add -- <cargo build …>")?;
    let mut args = args.to_vec();
    if !args.iter().any(|a| a.starts_with("--message-format")) {
        args.push(String::from("--message-format=json-render-diagnostics"));
    }
    let out = Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("the build: {}", e.kind()))?;
    if !out.status.success() {
        return Err(format!("the build failed ({})", out.status));
    }
    let units = units_of(&String::from_utf8_lossy(&out.stdout), artifact)?;
    let mut manifest = stamp(
        artifact,
        source,
        &config_args(command),
        &requested_features(command),
    )?;
    manifest.build = Some(Build {
        command: redacted(command),
        units,
        env: build_env(std::env::vars_os()),
    });
    Ok(manifest)
}

/// The wasm-opt flags and `RUSTFLAGS` a WASM build script passes: the
/// options of the line that runs `wasm-opt` (its command word, after any
/// `VAR=value`) up to its `-o`, and the `RUSTFLAGS="…"` value. A line that
/// only mentions it — `command -v wasm-opt`, `wasm-opt --version` — is no
/// recipe. CRLF line ends (a Windows checkout) read as LF, so a line that
/// continues with `\` still joins the next one.
pub(crate) fn script_recipe(script: &str) -> (Vec<String>, Option<String>) {
    let joined = script.replace("\r\n", "\n").replace("\\\n", " ");
    let flags = joined
        .lines()
        .find_map(|line| {
            let mut words = line.split_whitespace().skip_while(|w| w.contains('='));
            if words.next() != Some("wasm-opt") {
                return None;
            }
            let flags: Vec<String> = words
                .take_while(|w| *w != "-o")
                .filter(|w| w.starts_with('-'))
                .map(str::to_owned)
                .collect();
            (!flags.iter().any(|f| f == "--version")).then_some(flags)
        })
        .unwrap_or_default();
    let rustflags = joined
        .split("RUSTFLAGS=\"")
        .nth(1)
        .and_then(|rest| rest.split_once('"').map(|(value, _)| value.to_owned()));
    (flags, rustflags)
}

/// Run `command` (the WASM build script) and stamp the package it built in
/// `package`: the module's facts, the toolchain, `wasm-opt --version` from
/// `wasm_opt` (on the build's PATH), and the script's recipe.
pub(crate) fn stamp_wasm(
    package: &Path,
    source: &Path,
    command: &[String],
    wasm_opt: &str,
) -> Result<Manifest, String> {
    let (program, args) = command
        .split_first()
        .ok_or("stamp runs the build: add -- <the wasm build script>")?;
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("the build: {}", e.kind()))?;
    if !status.success() {
        return Err(format!("the build failed ({status})"));
    }
    let module = super::run::binary_of(package)?;
    let mut manifest = stamp(&module, source, &[], &[String::from("default")])?;
    let script = command
        .iter()
        .find(|a| Path::new(a).extension().is_some_and(|e| e == "sh"))
        .and_then(|a| std::fs::read(a).ok());
    let (wasm_opt_flags, rustflags) = script
        .as_deref()
        .map(|s| script_recipe(&String::from_utf8_lossy(s)))
        .unwrap_or_default();
    manifest.wasm_build = Some(WasmBuild {
        command: redacted(command),
        wasm_opt: super::env::probe(Command::new(wasm_opt).arg("--version")),
        wasm_opt_flags,
        rustflags,
        script_sha256: script.as_deref().map(external::sha256_bytes),
        env: build_env(std::env::vars_os()),
    });
    Ok(manifest)
}

/// Where the manifest of an artifact lives: `<file>.build.json`, or
/// `build.json` inside a package directory.
pub(crate) fn manifest_path(artifact: &Path) -> PathBuf {
    if artifact.is_dir() {
        artifact.join(PACKAGE_MANIFEST)
    } else {
        let mut name = artifact.as_os_str().to_owned();
        name.push(".build.json");
        PathBuf::from(name)
    }
}

/// Read a manifest. Errors name the file, never its directory: they can
/// reach a receipt, which carries no absolute path.
pub(crate) fn read(path: &Path) -> Result<Manifest, String> {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let text = std::fs::read_to_string(path).map_err(|e| format!("{name}: {}", e.kind()))?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|e| format!("{name}: {e}"))?;
    if manifest.format != FORMAT {
        return Err(format!(
            "{name}: format {:?}, expected {FORMAT}",
            manifest.format
        ));
    }
    Ok(manifest)
}

pub(crate) fn write(path: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// One parity finding of a base / candidate pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Mismatch {
    /// `identical` · `compiler` · `profile` · `features` · `config` ·
    /// `manifest` · `build` · `binaryen` · `worker_source` · `wasm_recipe` ·
    /// `loaded`.
    pub(crate) what: &'static str,
    pub(crate) detail: String,
}

/// One side of a pair: the file's sha256, the compiler commits it embeds,
/// and its manifest.
pub(crate) type Side<'a> = (&'a str, &'a [String], Option<&'a Manifest>);

/// What every pair shares: two different files, one compiler embedded in
/// both, and a manifest on each side that describes its file and was
/// stamped by a toolchain the file embeds. The manifests, when both exist.
fn common<'a>(
    (base_sha, base_commits, base_manifest): Side<'a>,
    (candidate_sha, candidate_commits, candidate_manifest): Side<'a>,
) -> (Vec<Mismatch>, Option<(&'a Manifest, &'a Manifest)>) {
    let mut found = Vec::new();
    if base_sha == candidate_sha {
        found.push(Mismatch {
            what: "identical",
            detail: format!("base and candidate are the same file (sha256 {base_sha})"),
        });
    }
    if base_commits.is_empty() || candidate_commits.is_empty() || base_commits != candidate_commits
    {
        found.push(Mismatch {
            what: "compiler",
            detail: format!(
                "embedded rustc commits differ: base [{}], candidate [{}]",
                base_commits.join(", "),
                candidate_commits.join(", ")
            ),
        });
    }
    let (Some(a), Some(b)) = (base_manifest, candidate_manifest) else {
        // a file that does not read as a current manifest (another format,
        // broken JSON) counts as none; its error is in the artifact's facts
        let usable = |m: Option<&Manifest>| {
            if m.is_some() {
                "usable"
            } else {
                "absent or unreadable"
            }
        };
        found.push(Mismatch {
            what: "manifest",
            detail: format!(
                "no usable build manifest: base {}, candidate {}",
                usable(base_manifest),
                usable(candidate_manifest)
            ),
        });
        return (found, None);
    };
    for (side, manifest, sha, commits) in [
        ("base", a, base_sha, base_commits),
        ("candidate", b, candidate_sha, candidate_commits),
    ] {
        if manifest.artifact.sha256 != sha {
            found.push(Mismatch {
                what: "manifest",
                detail: format!(
                    "{side}: the manifest describes another file (sha256 {})",
                    manifest.artifact.sha256
                ),
            });
        }
        match &manifest.rustc {
            Some(rustc) if commits.contains(&rustc.commit_hash) => {}
            Some(rustc) => found.push(Mismatch {
                what: "compiler",
                detail: format!(
                    "{side}: stamped with rustc {} ({}), which the binary does not embed",
                    rustc.release, rustc.commit_hash
                ),
            }),
            None => found.push(Mismatch {
                what: "compiler",
                detail: format!("{side}: no rustc recorded"),
            }),
        }
    }
    (found, Some((a, b)))
}

/// The variable names whose values differ between two environments.
fn env_differences(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> Option<String> {
    let names: BTreeSet<&String> = a
        .keys()
        .chain(b.keys())
        .filter(|k| a.get(*k) != b.get(*k))
        .collect();
    (!names.is_empty()).then(|| {
        names
            .into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// The builds two manifests observed: every role's unit compiled alike,
/// the same codegen environment.
fn build_parity(a: Option<&Build>, b: Option<&Build>) -> Vec<Mismatch> {
    let (Some(a), Some(b)) = (a, b) else {
        return vec![Mismatch {
            what: "manifest",
            detail: String::from(
                "a manifest not derived from a build: stamp the artifact with -- <its cargo build>",
            ),
        }];
    };
    let mut found = Vec::new();
    for role in ROLES {
        match (a.units.get(role), b.units.get(role)) {
            (Some(x), Some(y)) => {
                let profile = |u: &Unit| {
                    format!(
                        "opt-level {} debuginfo {} debug-assertions {} overflow-checks {}",
                        u.opt_level, u.debuginfo, u.debug_assertions, u.overflow_checks
                    )
                };
                if profile(x) != profile(y) {
                    found.push(Mismatch {
                        what: "profile",
                        detail: format!("{role} compiled with {} vs {}", profile(x), profile(y)),
                    });
                }
            }
            (None, None) => {}
            _ => found.push(Mismatch {
                what: "build",
                detail: format!("the {role} unit was observed on one side only"),
            }),
        }
    }
    if let Some(names) = env_differences(&a.env, &b.env) {
        found.push(Mismatch {
            what: "build",
            detail: format!("the build environments differ: {names}"),
        });
    }
    found
}

/// Parity of two native builds from their binaries and manifests:
/// [`common`], then the effective profiles, the features the commands
/// asked for, the non-profile overrides, and the builds observed. Empty
/// when the pair is fit for a comparison.
pub(crate) fn native_parity(base: Side<'_>, candidate: Side<'_>) -> Vec<Mismatch> {
    let (mut found, manifests) = common(base, candidate);
    let Some((a, b)) = manifests else {
        return found;
    };
    if a.profile != b.profile {
        let keys: BTreeSet<&String> = a.profile.keys().chain(b.profile.keys()).collect();
        let differs: Vec<String> = keys
            .into_iter()
            .filter(|k| a.profile.get(*k) != b.profile.get(*k))
            .map(|k| {
                format!(
                    "{k}: {} vs {}",
                    a.profile.get(k).map_or("-", String::as_str),
                    b.profile.get(k).map_or("-", String::as_str)
                )
            })
            .collect();
        found.push(Mismatch {
            what: "profile",
            detail: differs.join("; "),
        });
    }
    if a.features != b.features {
        found.push(Mismatch {
            what: "features",
            detail: format!(
                "requested [{}] vs [{}]",
                a.features.join(","),
                b.features.join(",")
            ),
        });
    }
    if a.other_config != b.other_config {
        found.push(Mismatch {
            what: "config",
            detail: format!(
                "non-profile overrides [{}] vs [{}]",
                a.other_config.join(" "),
                b.other_config.join(" ")
            ),
        });
    }
    found.extend(build_parity(a.build.as_ref(), b.build.as_ref()));
    found
}

/// Parity of two WASM packages: [`common`] — the compiler included — then
/// the binaryen that optimized them, its flags, and the build's `RUSTFLAGS`
/// and environment. The module recipe (producers, SIMD) is compared by the
/// caller.
pub(crate) fn wasm_parity(base: Side<'_>, candidate: Side<'_>) -> Vec<Mismatch> {
    let (mut found, manifests) = common(base, candidate);
    let Some((a, b)) = manifests else {
        return found;
    };
    let (Some(x), Some(y)) = (&a.wasm_build, &b.wasm_build) else {
        found.push(Mismatch {
            what: "manifest",
            detail: String::from(
                "a manifest not derived from a WASM build: stamp the package with --wasm -- <its build script>",
            ),
        });
        return found;
    };
    if x.wasm_opt.is_none() || x.wasm_opt != y.wasm_opt {
        found.push(Mismatch {
            what: "binaryen",
            detail: format!(
                "wasm-opt {} vs {}",
                x.wasm_opt.as_deref().unwrap_or("unknown"),
                y.wasm_opt.as_deref().unwrap_or("unknown")
            ),
        });
    }
    if x.wasm_opt_flags != y.wasm_opt_flags {
        found.push(Mismatch {
            what: "binaryen",
            detail: format!(
                "wasm-opt flags [{}] vs [{}]",
                x.wasm_opt_flags.join(" "),
                y.wasm_opt_flags.join(" ")
            ),
        });
    }
    if x.rustflags != y.rustflags {
        found.push(Mismatch {
            what: "build",
            detail: format!(
                "the build scripts' RUSTFLAGS differ: {:?} vs {:?}",
                x.rustflags, y.rustflags
            ),
        });
    }
    if let Some(names) = env_differences(&x.env, &y.env) {
        found.push(Mismatch {
            what: "build",
            detail: format!("the build environments differ: {names}"),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE_PROFILE: &str = "[profile.release]\nlto = \"thin\"\ncodegen-units = 1\nstrip = true\n\n\
                                     [profile.release.package.rxing]\noverflow-checks = true\n";

    fn profile(text: Option<&str>, overrides: &[&str]) -> BTreeMap<String, String> {
        let overrides: Vec<String> = overrides.iter().map(|s| (*s).to_owned()).collect();
        effective_profile(text, &overrides).expect("profile").0
    }

    #[test]
    fn rustc_version_output_parses() {
        let text = "rustc 1.97.0 (2d8144b78 2026-08-01)\nbinary: rustc\ncommit-hash: 2d8144b78805aaaa\n\
                    commit-date: 2026-08-01\nhost: aarch64-apple-darwin\nrelease: 1.97.0\nLLVM version: 21.1.0\n";
        assert_eq!(
            parse_rustc_vv(text),
            Some(Rustc {
                release: String::from("1.97.0"),
                commit_hash: String::from("2d8144b78805aaaa"),
                host: String::from("aarch64-apple-darwin"),
                llvm: String::from("21.1.0"),
            })
        );
        assert_eq!(parse_rustc_vv("garbage"), None);
    }

    #[test]
    fn compiler_commits_come_out_of_the_binary() {
        let a = "a".repeat(40);
        let b = "0123456789abcdef0123456789abcdef01234567";
        let blob = format!(
            "xx/rustc/{b}/library/core/src/x.rs\0/rustc/{a}/library/std\0/rustc/{a}/again\0/rustc/short/\0/rustc/{}/",
            "G".repeat(40)
        );
        assert_eq!(
            embedded_rustc_commits(blob.as_bytes()),
            vec![b.to_owned(), a.clone()],
            "sorted, distinct, 40 lowercase hex followed by a slash"
        );
        assert!(embedded_rustc_commits(b"no compiler here").is_empty());
    }

    /// The CLI pair: the published crate has no profile (cargo's defaults),
    /// the candidate overrides the workspace profile back to the defaults.
    /// Both effective profiles are the same map.
    #[test]
    fn profiles_compare_after_defaults_and_overrides() {
        let published = profile(None, &[]);
        let candidate = profile(
            Some(WORKSPACE_PROFILE),
            &[
                "profile.release.lto=false",
                "profile.release.codegen-units=16",
                "profile.release.strip=\"debuginfo\"",
                "profile.release.package.rxing.overflow-checks=false",
            ],
        );
        assert_eq!(published, candidate);
        assert_eq!(published.get("lto").map(String::as_str), Some("false"));
        assert_eq!(published.len(), RELEASE_DEFAULTS.len());

        let workspace = profile(Some(WORKSPACE_PROFILE), &[]);
        assert_eq!(workspace.get("lto").map(String::as_str), Some("\"thin\""));
        assert_eq!(
            workspace.get("codegen-units").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            workspace.get("strip").map(String::as_str),
            Some("\"symbols\"")
        );
        assert_eq!(
            workspace
                .get("package.rxing.overflow-checks")
                .map(String::as_str),
            Some("true")
        );
        assert_ne!(workspace, published);

        let (_, other) = effective_profile(
            None,
            &[String::from("build.rustflags=[\"-Ctarget-cpu=native\"]")],
        )
        .expect("profile");
        assert_eq!(
            other,
            vec![String::from("build.rustflags=[\"-Ctarget-cpu=native\"]")]
        );
        assert!(effective_profile(None, &[String::from("profile.release.lto")]).is_err());
    }

    fn manifest(
        sha: &str,
        commit: &str,
        profile_text: Option<&str>,
        overrides: &[&str],
    ) -> Manifest {
        let overrides: Vec<String> = overrides.iter().map(|s| (*s).to_owned()).collect();
        let (profile, other_config) = effective_profile(profile_text, &overrides).expect("profile");
        Manifest {
            format: FORMAT.to_owned(),
            artifact: ArtifactId {
                name: String::from("qrscan"),
                sha256: sha.to_owned(),
                bytes: 1,
            },
            rustc: Some(Rustc {
                release: String::from("1.97.0"),
                commit_hash: commit.to_owned(),
                host: String::new(),
                llvm: String::new(),
            }),
            embedded_rustc_commits: vec![commit.to_owned()],
            profile,
            config_overrides: overrides,
            other_config,
            features: vec![String::from("default")],
            lock: None,
            source: SourceId {
                kind: String::from("git"),
                origin: String::new(),
                sha: String::from("x"),
                tree: String::from("clean"),
            },
            stamped_utc: String::new(),
            build: Some(build()),
            wasm_build: None,
        }
    }

    fn whats(found: &[Mismatch]) -> Vec<&'static str> {
        found.iter().map(|m| m.what).collect()
    }

    /// Identical files, different compilers, different profiles, a missing
    /// or stale manifest — each refused by name.
    #[test]
    fn parity_refuses_identical_compiler_and_profile_mismatches() {
        let c1 = vec![String::from("1111111111111111111111111111111111111111")];
        let c2 = vec![String::from("2222222222222222222222222222222222222222")];
        let base = manifest("aa", &c1[0], Some(WORKSPACE_PROFILE), &[]);
        let candidate = manifest("bb", &c1[0], Some(WORKSPACE_PROFILE), &[]);
        assert!(
            native_parity(("aa", &c1, Some(&base)), ("bb", &c1, Some(&candidate))).is_empty(),
            "same compiler, same profile, two files: fit"
        );
        assert_eq!(
            whats(&native_parity(
                ("aa", &c1, Some(&base)),
                ("aa", &c1, Some(&base))
            )),
            vec!["identical"]
        );
        let other_compiler = manifest("bb", &c2[0], Some(WORKSPACE_PROFILE), &[]);
        assert_eq!(
            whats(&native_parity(
                ("aa", &c1, Some(&base)),
                ("bb", &c2, Some(&other_compiler))
            )),
            vec!["compiler"]
        );
        let thin_off = manifest(
            "bb",
            &c1[0],
            Some(WORKSPACE_PROFILE),
            &["profile.release.lto=false"],
        );
        let found = native_parity(("aa", &c1, Some(&base)), ("bb", &c1, Some(&thin_off)));
        assert_eq!(whats(&found), vec!["profile"]);
        assert_eq!(found[0].detail, "lto: \"thin\" vs false");
        let missing = native_parity(("aa", &c1, Some(&base)), ("bb", &c1, None));
        assert_eq!(whats(&missing), vec!["manifest"]);
        assert_eq!(
            missing[0].detail,
            "no usable build manifest: base usable, candidate absent or unreadable"
        );
        assert_eq!(
            whats(&native_parity(
                ("aa", &c1, Some(&base)),
                ("cc", &c1, Some(&candidate))
            )),
            vec!["manifest"],
            "a manifest of another file"
        );
        let mut stamped_elsewhere = candidate.clone();
        stamped_elsewhere.rustc = other_compiler.rustc.clone();
        assert_eq!(
            whats(&native_parity(
                ("aa", &c1, Some(&base)),
                ("bb", &c1, Some(&stamped_elsewhere))
            )),
            vec!["compiler"],
            "the stamp's toolchain is not the one the binary embeds"
        );
    }

    fn unit(package: &str, overflow_checks: bool) -> Unit {
        Unit {
            package: package.to_owned(),
            version: String::from("1.0.0"),
            opt_level: String::from("3"),
            debuginfo: String::from("0"),
            debug_assertions: false,
            overflow_checks,
            features: vec![String::from("default")],
        }
    }

    fn build() -> Build {
        Build {
            command: vec![String::from("cargo"), String::from("build")],
            units: ROLES
                .iter()
                .map(|role| ((*role).to_owned(), unit(role, *role == "rxing")))
                .collect(),
            env: BTreeMap::new(),
        }
    }

    /// The build command gives the overrides and the features asked for,
    /// package ids of every format name their package, the environment
    /// keeps its codegen variables (hashed), and paths never reach the
    /// record.
    #[test]
    fn build_commands_and_package_ids_are_read() {
        assert_eq!(
            package_of("registry+https://github.com/rust-lang/crates.io-index#rxing@0.9.1"),
            Some((String::from("rxing"), String::from("0.9.1")))
        );
        assert_eq!(
            package_of("path+file:///w/crates/qrcode-ai-scanner-cli#0.9.0"),
            Some((String::from("qrcode-ai-scanner-cli"), String::from("0.9.0")))
        );
        assert_eq!(
            package_of("path+file:///w/x#qrscan-bench-worker-base@0.0.0"),
            Some((
                String::from("qrscan-bench-worker-base"),
                String::from("0.0.0")
            ))
        );
        assert_eq!(
            package_of("serde 1.0.1 (registry+https://x)"),
            Some((String::from("serde"), String::from("1.0.1")))
        );
        let command: Vec<String> = [
            "/a/cargo-wrapper.sh",
            "cargo",
            "build",
            "--release",
            "--manifest-path",
            "/w/x/Cargo.toml",
            "--config",
            "profile.release.lto=false",
            "--config=profile.release.codegen-units=16",
            "-F",
            "b,a",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            config_args(&command),
            vec![
                String::from("profile.release.lto=false"),
                String::from("profile.release.codegen-units=16")
            ]
        );
        assert_eq!(requested_features(&command), vec!["a", "b"]);
        assert_eq!(requested_features(&command[..4]), vec!["default"]);
        assert_eq!(
            redacted(&command)[..6].to_vec(),
            vec![
                "…/cargo-wrapper.sh",
                "cargo",
                "build",
                "--release",
                "--manifest-path",
                "…/Cargo.toml"
            ]
        );
        let vars = [
            ("RUSTFLAGS", "-Ctarget-cpu=native"),
            ("CARGO_PROFILE_RELEASE_LTO", "false"),
            ("CARGO_BUILD_JOBS", "2"),
            ("HOME", "/somewhere"),
        ]
        .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)));
        let env = build_env(vars.into_iter());
        assert_eq!(
            env.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["CARGO_PROFILE_RELEASE_LTO", "RUSTFLAGS"],
            "codegen-affecting variables only, values hashed"
        );
        assert_eq!(
            env["RUSTFLAGS"],
            external::sha256_bytes(b"-Ctarget-cpu=native")
        );
    }

    /// A manifest's facts come from the build — cargo's JSON messages name
    /// the unit that produced the artifact and how each unit compiled;
    /// `stamp_build` runs the command and keeps what it saw; a failing
    /// build is refused.
    #[test]
    fn manifests_come_from_the_build() {
        let dir = std::env::temp_dir().join(format!("qrscan-bench-units-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let artifact = dir.join("qrscan");
        std::fs::write(&artifact, b"bin").expect("artifact");
        let message = |id: &str, kind: &str, overflow: bool, file: &str| {
            serde_json::json!({
                "reason": "compiler-artifact", "package_id": id,
                "target": {"kind": [kind], "name": "x"},
                "profile": {"opt_level": "3", "debuginfo": 0, "debug_assertions": false, "overflow_checks": overflow, "test": false},
                "features": ["default"], "filenames": [file], "executable": null, "fresh": true,
            })
            .to_string()
        };
        let stream = [
            message(
                "registry+https://x#rxing@0.9.1",
                "lib",
                true,
                "/t/librxing.rlib",
            ),
            message(
                "registry+https://x#serde@1.0.0",
                "lib",
                false,
                "/t/libserde.rlib",
            ),
            message(
                "path+file:///w#qrcode-ai-scanner@0.9.0",
                "lib",
                false,
                "/t/libq.rlib",
            ),
            message(
                "path+file:///w/qrcode-ai-scanner-cli#0.9.0",
                "bin",
                false,
                artifact.to_str().expect("utf-8"),
            ),
            String::from("{\"reason\":\"build-finished\",\"success\":true}"),
        ]
        .join("\n");
        let units = units_of(&stream, &artifact).expect("units");
        assert_eq!(
            units.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["artifact", "qrcode-ai-scanner", "rxing"]
        );
        assert_eq!(units["artifact"].package, "qrcode-ai-scanner-cli");
        assert!(units["rxing"].overflow_checks && !units["artifact"].overflow_checks);
        assert!(
            units_of(&stream, &dir).is_err_and(|e| e.contains("no unit")),
            "a build that produced another file"
        );

        // stamp_build runs the command and keeps what it saw
        let source = dir.join("src");
        std::fs::create_dir_all(&source).expect("source");
        std::fs::write(source.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
        let script = dir.join("fake-cargo.sh");
        std::fs::write(&script, format!("#!/bin/sh\ncat <<'EOF'\n{stream}\nEOF\n"))
            .expect("script");
        let fake: Vec<String> = [
            "sh",
            script.to_str().expect("utf-8"),
            "build",
            "--release",
            "--config",
            "profile.release.lto=false",
        ]
        .map(String::from)
        .to_vec();
        let stamped = stamp_build(&artifact, &source, &fake).expect("stamped");
        let built = stamped.build.as_ref().expect("a build record");
        assert_eq!(built.units, units);
        assert_eq!(stamped.config_overrides, vec!["profile.release.lto=false"]);
        assert_eq!(
            stamped.profile.get("lto").map(String::as_str),
            Some("false")
        );
        assert_eq!(stamped.features, vec!["default"]);
        assert!(
            built
                .command
                .iter()
                .all(|a| !a.contains('/') || a.starts_with("…/")),
            "paths are cut to their last component: {:?}",
            built.command
        );
        let failing: Vec<String> = ["sh", "-c", "exit 1"].map(String::from).to_vec();
        assert!(stamp_build(&artifact, &source, &failing).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A native pair whose observed builds differ — rxing without its
    /// overflow checks, another RUSTFLAGS, another feature request — or
    /// that was stamped without a build, is refused.
    #[test]
    fn parity_refuses_builds_that_differ() {
        let c1 = vec![String::from("1111111111111111111111111111111111111111")];
        let mut base = manifest("aa", &c1[0], Some(WORKSPACE_PROFILE), &[]);
        let mut candidate = manifest("bb", &c1[0], Some(WORKSPACE_PROFILE), &[]);
        base.build = Some(build());
        candidate.build = Some(build());
        let pair = |b: &Manifest| native_parity(("aa", &c1, Some(&base)), ("bb", &c1, Some(b)));
        assert!(pair(&candidate).is_empty(), "fit");
        let mut unchecked = candidate.clone();
        if let Some(b) = unchecked.build.as_mut() {
            b.units.insert(String::from("rxing"), unit("rxing", false));
        }
        let found = pair(&unchecked);
        assert_eq!(whats(&found), vec!["profile"]);
        assert_eq!(
            found[0].detail,
            "rxing compiled with opt-level 3 debuginfo 0 debug-assertions false overflow-checks true \
             vs opt-level 3 debuginfo 0 debug-assertions false overflow-checks false"
        );
        let mut flagged = candidate.clone();
        if let Some(b) = flagged.build.as_mut() {
            b.env.insert(String::from("RUSTFLAGS"), String::from("ab"));
        }
        assert_eq!(
            pair(&flagged)
                .iter()
                .map(|m| m.detail.as_str())
                .collect::<Vec<_>>(),
            vec!["the build environments differ: RUSTFLAGS"]
        );
        let mut all = candidate.clone();
        all.features = vec![String::from("--all-features")];
        assert_eq!(whats(&pair(&all)), vec!["features"]);
        let mut declared = candidate.clone();
        declared.build = None;
        assert_eq!(
            whats(&pair(&declared)),
            vec!["manifest"],
            "stamped without a build"
        );
    }

    fn wasm_manifest(sha: &str, commit: &str, wasm_opt: Option<&str>, flags: &[&str]) -> Manifest {
        let mut m = manifest(sha, commit, None, &[]);
        m.wasm_build = Some(WasmBuild {
            command: Vec::new(),
            wasm_opt: wasm_opt.map(str::to_owned),
            wasm_opt_flags: flags.iter().map(|f| (*f).to_owned()).collect(),
            rustflags: Some(String::from("-C target-feature=+simd128")),
            script_sha256: None,
            env: BTreeMap::new(),
        });
        m
    }

    /// A WASM pair is refused when its compilers differ (embedded or
    /// stamped), its binaryen version or flags differ, it is one module, or
    /// a side has no manifest of a build — as the published 0.9.0 package
    /// would be against a candidate built on another toolchain.
    #[test]
    fn wasm_parity_refuses_compiler_and_binaryen_differences() {
        let c1 = vec![String::from("1111111111111111111111111111111111111111")];
        let c2 = vec![String::from("2222222222222222222222222222222222222222")];
        let o3 = ["-O3", "--enable-simd", "--all-features"];
        let base = wasm_manifest(
            "aa",
            &c1[0],
            Some("wasm-opt version 130 (version_130)"),
            &o3,
        );
        let candidate = wasm_manifest(
            "bb",
            &c1[0],
            Some("wasm-opt version 130 (version_130)"),
            &o3,
        );
        let pair = |b: (&str, &[String], Option<&Manifest>)| {
            whats(&wasm_parity(("aa", &c1, Some(&base)), b))
        };
        assert!(pair(("bb", &c1, Some(&candidate))).is_empty(), "fit");
        assert_eq!(pair(("aa", &c1, Some(&base))), vec!["identical"]);
        let published = wasm_manifest(
            "bb",
            &c2[0],
            Some("wasm-opt version 130 (version_130)"),
            &o3,
        );
        assert_eq!(pair(("bb", &c2, Some(&published))), vec!["compiler"]);
        assert_eq!(pair(("bb", &c2, None)), vec!["compiler", "manifest"]);
        let newer = wasm_manifest("bb", &c1[0], Some("wasm-opt version 123"), &o3);
        assert_eq!(pair(("bb", &c1, Some(&newer))), vec!["binaryen"]);
        let o2 = wasm_manifest(
            "bb",
            &c1[0],
            Some("wasm-opt version 130 (version_130)"),
            &["-O2"],
        );
        assert_eq!(pair(("bb", &c1, Some(&o2))), vec!["binaryen"]);
        let unknown = wasm_manifest("bb", &c1[0], None, &o3);
        assert_eq!(pair(("bb", &c1, Some(&unknown))), vec!["binaryen"]);
        let mut native = candidate.clone();
        native.wasm_build = None;
        assert_eq!(pair(("bb", &c1, Some(&native))), vec!["manifest"]);
    }

    /// The release script's recipe, read from its text: wasm-opt's flags
    /// up to `-o`, and the RUSTFLAGS it builds with.
    #[test]
    fn the_wasm_build_script_recipe_is_read() {
        let script = std::fs::read_to_string(crate::repo_root().join("scripts/build-wasm.sh"))
            .expect("the release script");
        assert_eq!(
            script_recipe(&script),
            (
                vec![
                    String::from("-O3"),
                    String::from("--enable-simd"),
                    String::from("--all-features")
                ],
                Some(String::from("-C target-feature=+simd128"))
            )
        );
        assert_eq!(
            script_recipe("RUSTFLAGS=\"-C x\" b\r\nwasm-opt in.wasm \\\r\n  -O3 \\\r\n  -o o\r\n"),
            (vec![String::from("-O3")], Some(String::from("-C x"))),
            "a checkout with CRLF line ends reads the same recipe"
        );
        assert_eq!(script_recipe("echo nothing"), (Vec::new(), None));
        assert_eq!(
            script_recipe(
                "command -v wasm-opt >/dev/null\nwasm-opt --version\nA=1 wasm-opt in.wasm -Oz \\\n  -o out.wasm\n"
            ),
            (vec![String::from("-Oz")], None),
            "the probe and the version line are no recipe"
        );
    }

    /// `stamp --wasm` runs the build script, then records the module, the
    /// toolchain, `wasm-opt --version` and the script's recipe.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn wasm_packages_are_stamped_from_their_build() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir =
            std::env::temp_dir().join(format!("qrscan-bench-wasm-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let package = dir.join("pkg");
        std::fs::create_dir_all(&package).expect("dir");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"w\"\n").expect("manifest");
        // the recipe follows `exit 0`: read from the text, never run
        let script = dir.join("build-wasm.sh");
        std::fs::write(
            &script,
            format!(
                "printf '\\000asm\\001\\000\\000\\000' > {}/w_bg.wasm\n\
                 exit 0\n\
                 RUSTFLAGS=\"-C target-feature=+simd128\" wasm-pack build\n\
                 wasm-opt pkg/w_bg.wasm -O3 --enable-simd -o pkg/w.opt\n",
                package.display()
            ),
        )
        .expect("script");
        let fake_opt = dir.join("wasm-opt");
        std::fs::write(
            &fake_opt,
            "#!/bin/sh\necho 'wasm-opt version 130 (version_130)'\n",
        )
        .expect("fake");
        std::fs::set_permissions(&fake_opt, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let command = vec![
            String::from("sh"),
            script.to_str().expect("utf-8").to_owned(),
        ];
        let stamped = stamp_wasm(&package, &dir, &command, fake_opt.to_str().expect("utf-8"))
            .expect("stamped");
        let built = stamped.wasm_build.expect("a wasm build record");
        assert_eq!(stamped.artifact.name, "w_bg.wasm");
        assert_eq!(
            built.wasm_opt.as_deref(),
            Some("wasm-opt version 130 (version_130)")
        );
        assert_eq!(built.wasm_opt_flags, vec!["-O3", "--enable-simd"]);
        assert_eq!(
            built.rustflags.as_deref(),
            Some("-C target-feature=+simd128")
        );
        assert_eq!(built.command, vec!["sh", "…/build-wasm.sh"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_deltas_name_version_changes_and_count_the_rest() {
        let base = parse_lock(
            "version = 4\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n\
             [[package]]\nname = \"b\"\nversion = \"2.0.0\"\n[[package]]\nname = \"c\"\nversion = \"1.0.0\"\n",
        )
        .expect("lock");
        let candidate = parse_lock(
            "version = 4\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n\
             [[package]]\nname = \"b\"\nversion = \"2.1.0\"\n[[package]]\nname = \"d\"\nversion = \"0.1.0\"\n",
        )
        .expect("lock");
        assert_eq!(base.packages, vec!["a 1.0.0", "b 2.0.0", "c 1.0.0"]);
        assert_eq!(
            lock_delta(&base, &candidate),
            LockDelta {
                same: 1,
                version_differs: vec![String::from("b 2.0.0 -> 2.1.0")],
                only_base: 1,
                only_candidate: 1,
            }
        );
    }
}
