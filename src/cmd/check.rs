//! `forge check` — the authoring-time gate, with the dialect as its first
//! checker.
//!
//! # Why the command exists at all
//!
//! Phase 5 was written to "extend `forge check` to accept dialect sources"
//! and the command did not exist: 27 subcommands, no `check` (drift finding
//! D8). `FUTURE.md:771-782` proposes it as the TSX project's first move,
//! ahead of that compiler, on the reasoning that a checker closes the
//! authoring gap with none of the compiler work and that the same command
//! becomes the CI gate. Waiting for it would have left the dialect with no
//! authoring-time check for however long TSX takes; building the command here
//! means TSX's prop check lands into something that already exists, which is
//! strictly less work than the reverse (PHASE5-PROPOSAL §5, recommendation
//! ii).
//!
//! So: this builds the COMMAND and the dispatch. The dialect is its only
//! checker. It does not build the TSX checker.
//!
//! # No warning tier
//!
//! `FUTURE.md` asks that `forge check` FAIL where `contract_lint` only warns.
//! For the dialect the question has an easier answer than it does for props: a
//! refusal is already a hard failure at build time, so failing here costs
//! nothing, while a warning tier would be new surface with no entries to put
//! in it. Every finding is an error and any finding is a non-zero exit.
//!
//! # What it does not close
//!
//! Release skew. `forge check` closes the round-trip hole — you find out here
//! rather than after a push — which is a different and smaller thing than two
//! independently released binaries linking two revisions of the emitter. The
//! version stamp and its refusal are what close that, and they live with the
//! build (§4.2).

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use forge_lang_rustgen::{CompileError, Register, Tables, check_only};

use crate::dialect;

/// The versioned envelope `--format json` prints.
///
/// Each element of `sources` is one `forge-lang-diagnostics/1` object exactly
/// as `forge-lang --format json` emits it — same fields, same meanings — so a
/// parser written against that contract reads them unchanged. This wrapper
/// exists only because a workspace has many sources and that envelope has one
/// `path`. Additive-only, and the `N` bumps when a field is added
/// (`forge-lang/REJECTIONS.md`, the machine-envelope section).
const CHECK_SCHEMA: &str = "forge-check/1";

/// This machine could not do its job — the toolchain exit code, distinct from
/// a refusal so a CI step can tell "your source is wrong" from "my runner is".
/// `main` returns `Result`, which can only mean 0 or 1, so the third code is
/// spelled here.
const EXIT_TOOLCHAIN: i32 = 2;

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Workspace root (the directory holding `workspace.json`). Defaults to
    /// the current directory.
    #[arg(long)]
    manifest_dir: Option<PathBuf>,

    /// How diagnostics are reported: `human` (the rustc-shaped register on
    /// stderr) or `json` (the versioned envelope on stdout).
    #[arg(long, default_value = "human")]
    format: String,
}

pub fn run(args: CheckArgs) -> Result<()> {
    let json = match args.format.as_str() {
        "human" => false,
        "json" => true,
        other => anyhow::bail!("unknown --format `{other}` (expected `human` or `json`)"),
    };
    // NOT canonicalized. Every diagnostic names the source as the front end
    // resolved it, and a path the author can paste back — `../ws/domains/…`
    // rather than `/Users/…` — is the rustc convention and the one an editor
    // can click.
    let root = args.manifest_dir.unwrap_or_else(|| PathBuf::from("."));
    let registers = match check(&root) {
        Ok(r) => r,
        Err(message) => toolchain(message),
    };

    if registers.is_empty() {
        // Not a failure and not silence: a workspace with no dialect source
        // is a correct answer to the question that was asked, and an author
        // who expected one checked wants to know it found none.
        match json {
            true => println!("{}", envelope(&root, &registers)),
            false => eprintln!(
                "no dialect sources under {}/domains/*/services/*.py — nothing to check",
                root.display()
            ),
        }
        return Ok(());
    }

    let refused = registers.iter().filter(|r| !r.is_empty()).count();
    if json {
        println!("{}", envelope(&root, &registers));
    } else {
        for register in &registers {
            match register.is_empty() {
                true => eprintln!("{}: accepted", register.path),
                false => eprint!("{}", register.render()),
            }
        }
        eprintln!("{}", summary(&registers));
    }
    if refused == 0 {
        return Ok(());
    }
    // Exit 1, the refusal code, without a second error line: the register IS
    // the diagnostic and anyhow would print its own summary on top of it.
    flush();
    std::process::exit(1);
}

/// One verdict per dialect source in the workspace, in tree order.
///
/// `Err` is the toolchain's — this machine could not do its job — and never
/// the customer's source, which is always a [`Register`], empty when accepted.
/// Split from [`run`] so the verdicts are testable: `run` ends in
/// `process::exit`, which a test cannot survive.
fn check(root: &Path) -> Result<Vec<Register>, String> {
    if !root.join("workspace.json").exists() {
        return Err(format!(
            "no workspace.json at {} — `forge check` reads a workspace; pass \
             --manifest-dir or run from the workspace root",
            root.display()
        ));
    }
    let sources = dialect::sources(root);
    if sources.is_empty() {
        return Ok(Vec::new());
    }

    let schema_dirs = dialect::schema_roots(root);
    let tables = match schema_dirs.is_empty() {
        // Rule zero is off when a workspace declares nothing, exactly as
        // `forge-lang build` without `--schemas`: the schemas ADD checking,
        // their absence never changes what is accepted.
        true => None,
        false => Some(
            Tables::load(&schema_dirs)
                .map_err(|e| format!("cannot load the workspace's table schemas: {e}"))?,
        ),
    };

    // Every source is checked. Stopping at the first refusal would make a
    // second run necessary to see the second problem, which is the round trip
    // this command exists to remove.
    let mut registers: Vec<Register> = Vec::with_capacity(sources.len());
    for source in &sources {
        match check_only(source, tables.as_ref()) {
            Ok(label) => registers.push(Register::new(label)),
            Err(CompileError::Rejected(register)) => registers.push(register),
            Err(CompileError::Toolchain(m)) => {
                return Err(format!(
                    "{}: {m} (the dialect front end validates with CPython — is \
                     python3 on PATH?)",
                    source.display()
                ));
            }
            // `check_only` stops at the front end, so neither of these can
            // come back from it; the match stays exhaustive so a new variant
            // is a compile error here rather than a silent `_ => Ok`.
            Err(CompileError::NoOps(text) | CompileError::Conflict(text)) => {
                return Err(text.trim_end().to_string());
            }
        }
    }
    Ok(registers)
}

/// The closing line a person reads.
fn summary(registers: &[Register]) -> String {
    let refused: Vec<&Register> = registers.iter().filter(|r| !r.is_empty()).collect();
    let findings: usize = refused.iter().map(|r| r.rejections.len()).sum();
    format!(
        "{} source(s) checked, {} accepted, {} refused{}",
        registers.len(),
        registers.len() - refused.len(),
        refused.len(),
        match findings {
            0 => String::new(),
            n => format!(" ({n} finding(s))"),
        }
    )
}

/// The whole run as one JSON document.
fn envelope(root: &Path, registers: &[Register]) -> String {
    let doc = serde_json::json!({
        "schema_version": CHECK_SCHEMA,
        "root": root.display().to_string(),
        "accepted": registers.iter().filter(|r| r.is_empty()).count(),
        "refused": registers.iter().filter(|r| !r.is_empty()).count(),
        "sources": registers.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&doc).expect("the envelope is plain data")
}

/// Exit 2 — this toolchain or this machine, not the customer's source.
fn toolchain(message: String) -> ! {
    eprintln!("forge check: {message}");
    flush();
    std::process::exit(EXIT_TOOLCHAIN);
}

/// stdout is block-buffered when it is a pipe, and `process::exit` does not
/// run destructors — so a JSON envelope written just before an exit is a
/// document the caller never receives.
fn flush() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCEPTED: &str = r#"from dataclasses import dataclass

from forge import OpContext, Value, op


@dataclass
class Greeting:
    text: str


@op("hello")
def hello(ctx: OpContext, input: Value) -> Greeting:
    return Greeting(text="hi")
"#;

    /// `try/except` is FL0024 — outside the subset, and the register says so
    /// with the alternative rather than only the refusal.
    const REFUSED: &str = r#"from dataclasses import dataclass

from forge import OpContext, Value, op


@dataclass
class Greeting:
    text: str


@op("hello")
def hello(ctx: OpContext, input: Value) -> Greeting:
    try:
        return Greeting(text="hi")
    except ValueError:
        return Greeting(text="no")
"#;

    fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, body) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        dir
    }

    /// The front end validates with CPython. A machine without it says so
    /// rather than failing a gate about something else.
    fn skip_without_cpython(e: &str) -> bool {
        let missing = e.contains("python3") || e.contains("CPython");
        if missing {
            eprintln!("skipping: {e}");
        }
        missing
    }

    #[test]
    fn a_workspace_with_no_dialect_source_is_accepted_and_says_so() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/billing/service.json", "{}"),
        ]);
        assert!(check(ws.path()).unwrap().is_empty());
    }

    #[test]
    fn a_directory_that_is_not_a_workspace_is_the_toolchains_problem() {
        let dir = tempfile::tempdir().unwrap();
        let err = check(dir.path()).unwrap_err();
        assert!(err.contains("no workspace.json"), "{err}");
    }

    #[test]
    fn every_source_is_checked_and_the_refusals_carry_their_codes() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/a/services/hello.py", ACCEPTED),
            ("domains/b/services/hello.py", REFUSED),
        ]);
        let registers = match check(ws.path()) {
            Ok(r) => r,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        // BOTH, not the first: a checker that stops at the first refusal makes
        // the author run it again to see the second one.
        assert_eq!(registers.len(), 2);
        assert!(registers[0].is_empty(), "domains/a should be accepted");
        assert_eq!(registers[1].rejections.len(), 1);
        assert!(
            registers[1].render().contains("FL0024"),
            "{}",
            registers[1].render()
        );
        assert_eq!(
            summary(&registers),
            "2 source(s) checked, 1 accepted, 1 refused (1 finding(s))"
        );
    }

    /// The machine surface: one `forge-check/1` wrapper whose `sources` are
    /// the `forge-lang-diagnostics/1` objects a parser already knows.
    #[test]
    fn the_json_envelope_nests_the_per_source_contract_unchanged() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/b/services/hello.py", REFUSED),
        ]);
        let registers = match check(ws.path()) {
            Ok(r) => r,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        let doc: serde_json::Value =
            serde_json::from_str(&envelope(ws.path(), &registers)).unwrap();
        assert_eq!(doc["schema_version"], CHECK_SCHEMA);
        assert_eq!(doc["accepted"], 0);
        assert_eq!(doc["refused"], 1);
        let source = &doc["sources"][0];
        assert_eq!(source["schema_version"], "forge-lang-diagnostics/1");
        assert_eq!(source["diagnostics"][0]["code"], "FL0024");
        assert_eq!(source["diagnostics"][0]["severity"], "error");
    }

    /// Rule zero, through the command: a table no loaded schema declares is
    /// FL0078, and the schema scope that finds it is the WHOLE workspace —
    /// this source is in `b` and the table it reads is declared by `a`.
    #[test]
    fn the_schema_scope_spans_domains() {
        const READS_ANOTHER_DOMAINS_TABLE: &str = r#"from dataclasses import dataclass

from forge import OpContext, Value, op, storage


@dataclass
class Count:
    rows: int


@op("count_tenants")
def count_tenants(ctx: OpContext, input: Value) -> Count:
    rows: list[Value] = storage.rows(storage.query({"from": "tenants", "limit": 10}))
    return Count(rows=len(rows))
"#;
        const TENANTS_TABLE: &str = r#"{
  "name": "tenants",
  "columns": [{"name": "id", "type": "text", "nullable": false}]
}"#;
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/a/schemas/tenants.table.json", TENANTS_TABLE),
            ("domains/b/services/count.py", READS_ANOTHER_DOMAINS_TABLE),
        ]);
        let registers = match check(ws.path()) {
            Ok(r) => r,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(registers.len(), 1);
        assert!(
            registers[0].is_empty(),
            "a domain reading another domain's declared table must be accepted:\n{}",
            registers[0].render()
        );
    }
}
