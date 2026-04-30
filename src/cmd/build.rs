//! `forge build` — compile a Rust crate to a workspace-deployable
//! WASM module.
//!
//! Wraps `cargo build --release --target wasm32-wasip2`. The result
//! lands at `target/wasm32-wasip2/release/<crate>.wasm`. v1 doesn't
//! do post-processing (no `wasm-opt`, no component-model packaging
//! beyond what wasm32-wasip2 already produces) — those are
//! H2-territory optimizations.
//!
//! v1 also intentionally leaves the toolchain installation as a
//! prerequisite — `rustup target add wasm32-wasip2` is the customer's
//! responsibility. The CLI surfaces a clear error when the target
//! isn't installed; we don't try to install it for them.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use clap::Args;

#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Path to the crate (must contain `Cargo.toml`). Defaults to
    /// the current working directory.
    #[arg(long)]
    manifest_dir: Option<PathBuf>,

    /// Build profile. Defaults to `release` because debug WASM is
    /// 5-10× larger and not what gets deployed. Set `--profile debug`
    /// for fast iteration where size doesn't matter.
    #[arg(long, default_value = "release")]
    profile: String,
}

pub async fn run(args: BuildArgs) -> Result<()> {
    let manifest_dir = args
        .manifest_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("resolving --manifest-dir")?;

    let manifest = manifest_dir.join("Cargo.toml");
    if !manifest.exists() {
        anyhow::bail!(
            "no Cargo.toml at {} — pass --manifest-dir or run from a Rust crate root",
            manifest.display(),
        );
    }

    eprintln!(
        "building {} for wasm32-wasip2 ({})",
        manifest_dir.display(),
        args.profile,
    );

    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("--target").arg("wasm32-wasip2");
    if args.profile == "release" {
        cmd.arg("--release");
    } else if args.profile != "debug" {
        cmd.arg("--profile").arg(&args.profile);
    }
    cmd.current_dir(&manifest_dir);
    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

    let status = cmd
        .status()
        .with_context(|| "failed to spawn `cargo build` — is cargo on PATH?")?;
    if !status.success() {
        // cargo's stderr is already streamed to the user's terminal;
        // we just need to surface the exit code as a CLI error.
        anyhow::bail!("cargo build failed (exit {status})");
    }

    // Locate the resulting `.wasm`. Cargo puts it at
    // target/wasm32-wasip2/{release|debug}/<crate>.wasm; the crate
    // name comes from the workspace's Cargo.toml `[package].name`
    // (read directly to avoid a cargo-metadata roundtrip).
    let crate_name = read_crate_name(&manifest)?;
    let profile_dir = args.profile.as_str();
    let wasm_path = manifest_dir
        .join("target")
        .join("wasm32-wasip2")
        .join(profile_dir)
        .join(format!("{crate_name}.wasm"));
    if !wasm_path.exists() {
        anyhow::bail!(
            "build succeeded but expected output not found at {}",
            wasm_path.display(),
        );
    }
    let size = std::fs::metadata(&wasm_path).map(|m| m.len()).unwrap_or(0);

    println!("{}", wasm_path.display());
    eprintln!("size: {} bytes ({:.1} KiB)", size, size as f64 / 1024.0);
    Ok(())
}

/// Pull `[package].name` out of a `Cargo.toml` without going through
/// `cargo metadata` (which is slow and pulls in its own toolchain).
fn read_crate_name(manifest: &std::path::Path) -> Result<String> {
    let bytes = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let parsed: toml::Value =
        toml::from_str(&bytes).with_context(|| format!("parsing {}", manifest.display()))?;
    let name = parsed
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} is missing [package].name; is this a workspace root? \
                 Pass --manifest-dir to a member crate.",
                manifest.display(),
            )
        })?;
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_crate_name_handles_well_formed_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        std::fs::write(
            &path,
            r#"
[package]
name = "my-wasm-crate"
version = "0.1.0"
edition = "2024"
"#,
        )
        .unwrap();
        assert_eq!(read_crate_name(&path).unwrap(), "my-wasm-crate");
    }

    #[test]
    fn read_crate_name_rejects_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "[workspace]\nmembers = [\"foo\"]").unwrap();
        let err = read_crate_name(&path).unwrap_err();
        assert!(format!("{err:#}").contains("missing [package].name"));
    }
}
