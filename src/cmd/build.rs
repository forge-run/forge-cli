//! `forge build` — compile a Rust crate to a workspace-deployable
//! WASM module.
//!
//! Wraps `cargo build --release --target wasm32-wasip1`, then
//! repackages the resulting cdylib as a Component-Model artifact
//! via `wit-component` with the WASI Preview-1 reactor adapter.
//! Required since forge-runtime's W3 cutover (2026-05-15) — the
//! runtime refuses core modules and only instantiates Components.
//!
//! The result lands at `target/wasm32-wasip1/release/<crate>.wasm`
//! (in-place replacement of the raw cdylib). `forge deploy`
//! consumes the same path.
//!
//! v1 intentionally leaves the toolchain installation as a
//! prerequisite — `rustup target add wasm32-wasip1` is the customer's
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
        "building {} for wasm32-wasip1 ({})",
        manifest_dir.display(),
        args.profile,
    );

    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("--target").arg("wasm32-wasip1");
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
    // target/wasm32-wasip1/{release|debug}/<crate>.wasm; the crate
    // name comes from the workspace's Cargo.toml `[package].name`
    // (read directly to avoid a cargo-metadata roundtrip).
    let crate_name = read_crate_name(&manifest)?;
    let profile_dir = args.profile.as_str();
    // Cargo replaces `-` with `_` in artifact filenames. A crate
    // named `my-app` produces `my_app.wasm`, not `my-app.wasm`.
    // Mirror the transform so the validator looks where cargo
    // actually wrote the file.
    let artifact_name = crate_name.replace('-', "_");
    let wasm_path = manifest_dir
        .join("target")
        .join("wasm32-wasip1")
        .join(profile_dir)
        .join(format!("{artifact_name}.wasm"));
    if !wasm_path.exists() {
        anyhow::bail!(
            "build succeeded but expected output not found at {}",
            wasm_path.display(),
        );
    }
    let bytes =
        std::fs::read(&wasm_path).with_context(|| format!("reading {}", wasm_path.display()))?;
    validate_wasm_artifact(&bytes)?;

    // Wrap the core cdylib as a Component-Model artifact. Skip if
    // the bytes are already a Component (header layer byte at offset
    // 4 is `0d`; core cdylib's is `01`) — re-running `forge build`
    // shouldn't double-wrap.
    let final_bytes = if is_component(&bytes) {
        eprintln!("already a Component — skipping wit-component wrap");
        bytes
    } else {
        eprintln!("wrapping cdylib as Component (wit-component)");
        let wrapped = wrap_as_component(&bytes).context("wit-component wrap")?;
        std::fs::write(&wasm_path, &wrapped)
            .with_context(|| format!("writing {}", wasm_path.display()))?;
        wrapped
    };

    let size = final_bytes.len();
    println!("{}", wasm_path.display());
    eprintln!("size: {} bytes ({:.1} KiB)", size, size as f64 / 1024.0);
    if size as u64 > WARN_SIZE_BYTES {
        eprintln!(
            "warning: module is over {} MiB; deploys above {} MiB will be rejected.",
            WARN_SIZE_BYTES / 1_048_576,
            HARD_SIZE_BYTES / 1_048_576,
        );
    }
    Ok(())
}

/// Header byte at offset 4 distinguishes Component (0x0d) from
/// core module (0x01). The wasm magic at bytes 0..4 is `\0asm` in
/// both; the next 4 bytes encode `version | layer` little-endian.
/// Core: `01 00 00 00`. Component: `0d 00 01 00`.
fn is_component(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[4..8] == [0x0d, 0x00, 0x01, 0x00]
}

/// Re-package a wasm32-wasip1 cdylib as a Component using
/// `wit-component` with the WASI Preview-1 reactor adapter (which
/// satisfies the wasi_snapshot_preview1 imports the cdylib makes).
/// Adapter blob ships inside `wasi-preview1-component-adapter-provider`.
fn wrap_as_component(core: &[u8]) -> Result<Vec<u8>> {
    let adapter = wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_REACTOR_ADAPTER;
    let bytes = wit_component::ComponentEncoder::default()
        .module(core)
        .context("core wasm has no wit-bindgen metadata — make sure the crate uses forge-sdk-v2")?
        .adapter("wasi_snapshot_preview1", adapter)
        .context("attach WASI Preview-1 reactor adapter")?
        .encode()
        .context("encode Component")?;
    Ok(bytes)
}

/// Soft cap — over this we warn but still let the customer deploy.
const WARN_SIZE_BYTES: u64 = 10 * 1_048_576;
/// Hard cap — manage_routes::wasm_deploy_handler rejects over this.
const HARD_SIZE_BYTES: u64 = 25 * 1_048_576;

/// Validate the freshly-built `.wasm` is actually a wasm module
/// before letting the customer waste a deploy roundtrip on garbage
/// bytes. Surfaces three failure modes in priority order:
///
/// 1. Empty / missing magic header → not a wasm file at all.
///    The most common shape of "I built the wrong target".
/// 2. Wrong wasm version → forge-runtime won't load it.
/// 3. No exports → forge-storage's user_service_validator will
///    reject it on deploy ("service has no exports"). Catch here so
///    the error lands at build time, not after upload.
///
/// We don't WIT-validate (would require tracking the host imports
/// against the crate's compiled component-model interface);
/// forge-runtime's wasmtime layer surfaces those at instantiation,
/// not deploy. Adding WIT validation here is H2 territory.
fn validate_wasm_artifact(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 8 {
        anyhow::bail!(
            "output is not a wasm module — too short ({} bytes). \
             Did `cargo build` write to a different path? \
             Re-run with --profile debug to see the cargo output.",
            bytes.len(),
        );
    }
    // wasm magic = `\0asm`, followed by 4-byte little-endian version.
    if &bytes[0..4] != b"\0asm" {
        anyhow::bail!(
            "output is not a wasm module — missing `\\0asm` header. \
             Make sure the crate's [lib] section sets `crate-type = [\"cdylib\"]` \
             and that you're targeting wasm32-wasip1.",
        );
    }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if version != 1 {
        anyhow::bail!(
            "wasm module declares version {version}; forge-runtime expects version 1. \
             Toolchain mismatch — re-run `rustup update` and retry.",
        );
    }
    if !has_export_section(bytes) {
        anyhow::bail!(
            "wasm module has no exports — forge-runtime won't be able to invoke any \
             operations against this module. Make sure your crate exports its handler \
             functions (e.g. `#[no_mangle] pub extern \"C\" fn forge_op(...)`) and that \
             they're not optimized away by lto.",
        );
    }
    Ok(())
}

/// Walk the wasm sections looking for one with id 7 (Export).
/// Caller has already confirmed the magic + version (8 bytes); we
/// pick up at offset 8. v1 doesn't decode the exports — we just
/// need to know whether at least one is declared.
///
/// WASM section header is `[id: u8][size: u32-leb128][body...]`.
/// Section ids: 0=custom, 1=type, 2=import, 3=function, 4=table,
/// 5=memory, 6=global, 7=export, 8=start, 9=element, ... See
/// `https://webassembly.github.io/spec/core/binary/modules.html`.
fn has_export_section(bytes: &[u8]) -> bool {
    let mut cursor = 8usize;
    while cursor < bytes.len() {
        let section_id = bytes[cursor];
        cursor += 1;
        let (size, leb_len) = match read_leb128_u32(&bytes[cursor..]) {
            Some(v) => v,
            None => return false,
        };
        cursor += leb_len;
        if section_id == 7 {
            // Export section exists. v1 doesn't dig further — an
            // empty export section is technically valid wasm but
            // never produced by rustc; treating presence-of-section
            // as proof-of-export is the right tradeoff for build-time
            // validation.
            return true;
        }
        cursor += size as usize;
    }
    false
}

/// Decode a single ULEB128 u32 from the head of `bytes`. Returns
/// `(value, bytes_consumed)` or `None` on overflow / overflow.
/// The wasm spec caps section-size LEB128s at 5 bytes.
fn read_leb128_u32(bytes: &[u8]) -> Option<(u32, usize)> {
    let mut result: u32 = 0;
    let mut shift = 0;
    for (i, &b) in bytes.iter().take(5).enumerate() {
        result |= ((b & 0x7f) as u32).checked_shl(shift)?;
        if b & 0x80 == 0 {
            return Some((result, i + 1));
        }
        shift += 7;
    }
    None
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

    #[test]
    fn validate_wasm_rejects_empty_bytes() {
        let err = validate_wasm_artifact(&[]).unwrap_err();
        assert!(format!("{err:#}").contains("too short"));
    }

    #[test]
    fn validate_wasm_rejects_wrong_magic() {
        // ELF magic — what `cargo build` produces if the customer
        // forgot to pass `--target wasm32-wasip1`.
        let mut bytes = vec![0x7f, b'E', b'L', b'F'];
        bytes.extend_from_slice(&[0; 100]);
        let err = validate_wasm_artifact(&bytes).unwrap_err();
        assert!(format!("{err:#}").contains("missing `\\0asm`"));
    }

    #[test]
    fn validate_wasm_rejects_wrong_version() {
        // Magic + version=2 (which doesn't exist; spec is v1).
        let bytes = [b'\0', b'a', b's', b'm', 2, 0, 0, 0];
        let err = validate_wasm_artifact(&bytes).unwrap_err();
        assert!(format!("{err:#}").contains("version"));
    }

    #[test]
    fn validate_wasm_accepts_a_module_with_exports() {
        // Use wat to produce a known-good module with at least one
        // export. Same library forge-runtime tests use for fixture
        // wasm.
        let bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "noop")))
        "#,
        )
        .unwrap();
        validate_wasm_artifact(&bytes).expect("module with exports should pass");
    }

    #[test]
    fn validate_wasm_rejects_module_without_exports() {
        let bytes = wat::parse_str("(module (memory 1))").unwrap();
        let err = validate_wasm_artifact(&bytes).unwrap_err();
        assert!(format!("{err:#}").contains("no exports"));
    }
}
