//! `sync-version` — one source, every publish surface.
//!
//! The workspace `Cargo.toml` version is the SOURCE; the excluded
//! crates, the foreign-toolchain manifests, the cli's crates.io core
//! pin and the version-pinned doc coordinates cannot inherit it, so
//! releases hand-bumped eight files (RELEASING.md §1) and the gradle
//! comment promised this command for two releases. `--check` verifies
//! only (exit 1 on drift) — the local twin of mobile.yml's tag gate.
//! Cargo lockfiles stay YOURS: the command prints their regeneration list.
//! The Flutter example's local path dependency identity is mirrored here;
//! its hosted dependency resolution remains untouched. No cargo invocations.

use std::path::Path;

/// One managed surface: the file plus the line-level rewrite rule.
struct Surface {
    path: &'static str,
    /// Line qualifies when it starts with this (post-trim).
    starts: &'static str,
    /// Rewrite the qualifying line for `version` (only the FIRST
    /// qualifying line is touched — every surface spells one).
    render: fn(version: &str, line: &str) -> String,
}

const SURFACES: &[Surface] = &[
    Surface {
        path: "crates/qrcode-ai-scanner-cli/Cargo.toml",
        starts: "qrcode-ai-scanner = { version =",
        render: |v, line| {
            // keep everything after the version string (path, features)
            match line.split_once("\", ") {
                Some((_, tail)) => {
                    format!("qrcode-ai-scanner = {{ version = \"{v}\", {tail}")
                }
                None => format!("qrcode-ai-scanner = {{ version = \"{v}\" }}"),
            }
        },
    },
    Surface {
        path: "crates/qrcode-ai-scanner-py/Cargo.toml",
        starts: "version = \"",
        render: |v, _| format!("version = \"{v}\""),
    },
    Surface {
        path: "crates/qrcode-ai-scanner-uniffi/Cargo.toml",
        starts: "version = \"",
        render: |v, _| format!("version = \"{v}\""),
    },
    Surface {
        path: "bindings/flutter/rust/Cargo.toml",
        starts: "version = \"",
        render: |v, _| format!("version = \"{v}\""),
    },
    Surface {
        path: "bindings/kotlin/qrcodeaiscanner/build.gradle.kts",
        starts: "version = \"",
        render: |v, _| format!("version = \"{v}\""),
    },
    Surface {
        path: "bindings/flutter/pubspec.yaml",
        starts: "version:",
        render: |v, _| format!("version: {v}"),
    },
    Surface {
        path: "crates/qrcode-ai-scanner-node/package.json",
        starts: "\"version\":",
        render: |v, _| format!("  \"version\": \"{v}\","),
    },
    // Version-pinned doc coordinates (the "left at v0.6.0 through two
    // releases" class — RELEASING.md §1 makes them ride the same commit).
    Surface {
        path: "bindings/kotlin/README.md",
        starts: "implementation(\"com.github.supernovae-st:qrcode-ai-scanner:v",
        render: |v, _| {
            format!("    implementation(\"com.github.supernovae-st:qrcode-ai-scanner:v{v}\")")
        },
    },
    Surface {
        path: "README.md",
        starts: "(`com.github.supernovae-st:qrcode-ai-scanner:v",
        render: |v, _| {
            format!("(`com.github.supernovae-st:qrcode-ai-scanner:v{v}` — use the latest tag).")
        },
    },
];

/// Workspace version — the single source every surface mirrors.
fn workspace_version() -> String {
    let root = std::fs::read_to_string("Cargo.toml").expect("workspace Cargo.toml");
    let pkg = root
        .split("[workspace.package]")
        .nth(1)
        .expect("[workspace.package] section");
    pkg.lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("version = \"")
                .and_then(|r| r.split('"').next())
        })
        .expect("workspace version line")
        .to_string()
}

/// Apply (or verify) one surface. Returns the line that was (or would
/// be) rewritten, or `None` when already in sync.
fn sync_surface(surface: &Surface, version: &str, write: bool) -> Option<(String, String)> {
    let text =
        std::fs::read_to_string(surface.path).unwrap_or_else(|e| panic!("{}: {e}", surface.path));
    let mut out = Vec::with_capacity(text.lines().count());
    let mut hit: Option<(String, String)> = None;
    for line in text.lines() {
        if hit.is_none() && line.trim_start().starts_with(surface.starts) {
            let indent = &line[..line.len() - line.trim_start().len()];
            let rendered = (surface.render)(version, line.trim_start());
            // package.json / kotlin README carry their own indentation
            // in the rendered form; plain TOML/YAML keep the line's.
            let new_line = if rendered.starts_with(' ') {
                rendered
            } else {
                format!("{indent}{rendered}")
            };
            if new_line != line {
                hit = Some((line.to_string(), new_line.clone()));
            }
            out.push(new_line);
        } else {
            out.push(line.to_string());
        }
    }
    if let Some((old, new)) = &hit {
        if write {
            let trailing = if text.ends_with('\n') { "\n" } else { "" };
            std::fs::write(surface.path, out.join("\n") + trailing)
                .unwrap_or_else(|e| panic!("{}: {e}", surface.path));
        }
        println!(
            "  {}\n    - {}\n    + {}",
            surface.path,
            old.trim(),
            new.trim()
        );
    }
    hit
}

/// Mirror only the local package identity, preserving every resolved dependency.
fn flutter_example_lock(text: &str, version: &str) -> String {
    let mut inside = false;
    let mut found = 0;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with("  ") && !line.starts_with("    ") && line.ends_with(':') {
            inside = line == "  qrcode_ai_scanner:";
        }
        if inside && line.starts_with("    version:") {
            out.push(format!("    version: \"{version}\""));
            found += 1;
        } else {
            out.push(line.to_owned());
        }
    }
    assert_eq!(
        found, 1,
        "Flutter example local package version must be unique"
    );
    out.join("\n") + if text.ends_with('\n') { "\n" } else { "" }
}

/// Entry point: `xtask sync-version [--check]`.
pub fn run(check_only: bool) {
    let version = workspace_version();
    println!(
        "sync-version · workspace {version} · {} surfaces{}",
        SURFACES.len() + 1,
        if check_only { " · --check" } else { "" }
    );
    let mut drifted = 0usize;
    for surface in SURFACES {
        assert!(
            Path::new(surface.path).exists(),
            "{} moved — update SURFACES",
            surface.path
        );
        if sync_surface(surface, &version, !check_only).is_some() {
            drifted += 1;
        }
    }
    let path = "bindings/flutter/example/pubspec.lock";
    let original = std::fs::read_to_string(path).expect("Flutter example lockfile");
    let updated = flutter_example_lock(&original, &version);
    if updated != original {
        drifted += 1;
        println!("  {path} · local package identity → {version}");
        if !check_only {
            std::fs::write(path, updated).expect("Flutter example lockfile write");
        }
    }
    if drifted == 0 {
        println!("  every surface already at {version}");
    } else if check_only {
        eprintln!("::error::{drifted} surface(s) drift from workspace {version}");
        std::process::exit(1);
    } else {
        println!(
            "  rewrote {drifted} surface(s) · now regenerate the lockfiles:\n    \
             for m in Cargo.toml crates/qrcode-ai-scanner-py/Cargo.toml \
             crates/qrcode-ai-scanner-uniffi/Cargo.toml bindings/flutter/rust/Cargo.toml \
             fuzz/Cargo.toml; do cargo update -w --manifest-path $m --offline; done"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flutter_local_identity_preserves_hosted_resolution_and_is_idempotent() {
        let text = "packages:\n  before:\n    source: hosted\n    version: \"2.3.4\"\n  qrcode_ai_scanner:\n    source: path\n    version: \"0.9.0\"\n  after:\n    source: hosted\n    version: \"5.6.7\"\n";
        let expected = text.replace("version: \"0.9.0\"", "version: \"0.10.0\"");
        assert_eq!(flutter_example_lock(text, "0.10.0"), expected);
        assert_eq!(flutter_example_lock(&expected, "0.10.0"), expected);
    }

    #[test]
    fn cli_pin_render_keeps_path_and_features() {
        let line = r#"qrcode-ai-scanner = { version = "0.8.1", path = "../qrcode-ai-scanner", features = ["serde"] }"#;
        let s = &SURFACES[0];
        assert!(line.starts_with(s.starts));
        let out = (s.render)("0.9.0", line);
        assert_eq!(
            out,
            r#"qrcode-ai-scanner = { version = "0.9.0", path = "../qrcode-ai-scanner", features = ["serde"] }"#
        );
    }

    #[test]
    fn every_render_is_idempotent_at_same_version() {
        let samples = [
            r#"qrcode-ai-scanner = { version = "1.2.3", path = "../qrcode-ai-scanner", features = ["serde"] }"#,
            r#"version = "1.2.3""#,
            r#"version = "1.2.3""#,
            r#"version = "1.2.3""#,
            r#"version = "1.2.3""#,
            "version: 1.2.3",
            r#"  "version": "1.2.3","#,
            r#"    implementation("com.github.supernovae-st:qrcode-ai-scanner:v1.2.3")"#,
            r"(`com.github.supernovae-st:qrcode-ai-scanner:v1.2.3` — use the latest tag).",
        ];
        for (surface, sample) in SURFACES.iter().zip(samples) {
            let rendered = (surface.render)("1.2.3", sample.trim_start());
            assert_eq!(
                rendered.trim_start(),
                sample.trim_start(),
                "{} render not idempotent",
                surface.path
            );
        }
    }
}
