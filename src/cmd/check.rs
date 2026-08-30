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

use std::collections::BTreeMap;
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
    let found = match check(&root) {
        Ok(v) => v,
        Err(message) => toolchain(message),
    };

    if found.is_empty() {
        // Not a failure and not silence: a workspace with no dialect source
        // is a correct answer to the question that was asked, and an author
        // who expected one checked wants to know it found none.
        match json {
            true => println!("{}", envelope(&root, &found)),
            false => eprintln!(
                "no dialect sources under {}/domains/*/services/ — nothing to check \
                 (a dialect source is a .py or a .ts file)",
                root.display()
            ),
        }
        return Ok(());
    }

    if json {
        println!("{}", envelope(&root, &found));
    } else {
        for register in &found.registers {
            match register.is_empty() {
                true => eprintln!("{}: accepted", register.path),
                false => eprint!("{}", register.render()),
            }
        }
        for m in &found.mismatches {
            eprintln!("error: {}", m.render());
        }
        eprintln!("{}", summary(&found));
    }
    if found.ok() {
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
fn check(root: &Path) -> Result<Verdicts, String> {
    if !root.join("workspace.json").exists() {
        return Err(format!(
            "no workspace.json at {} — `forge check` reads a workspace; pass \
             --manifest-dir or run from the workspace root",
            root.display()
        ));
    }
    let sources = dialect::sources(root);
    if sources.is_empty() {
        return Ok(Verdicts::default());
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
    // Op names per domain, collected from the SAME front-end pass that
    // produces the verdicts — a second pass would be a second chance to
    // disagree about the same file.
    let mut defined: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for source in &sources {
        match check_only(source, tables.as_ref()) {
            Ok(accepted) => {
                if let Some(domain) = domain_of(root, source) {
                    defined.entry(domain).or_default().extend(accepted.ops);
                }
                registers.push(Register::new(accepted.source));
            }
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
    // Only meaningful when every source was accepted: a refused source
    // defines no ops as far as the front end is concerned, so every op it
    // would have defined would be reported as "declared and not defined" —
    // a second, wrong diagnosis stacked on the real one.
    let mismatches = match registers.iter().all(|r| r.is_empty()) {
        true => dialect::op_mismatches(root, &defined),
        false => Vec::new(),
    };
    Ok(Verdicts {
        registers,
        mismatches,
    })
}

/// Which domain a source belongs to — `domains/<d>/services/x.py` -> `<d>`.
fn domain_of(root: &Path, source: &Path) -> Option<String> {
    source
        .strip_prefix(root)
        .ok()?
        .components()
        .nth(1)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
}

/// What one run of `forge check` found.
#[derive(Debug, Default)]
struct Verdicts {
    /// One per dialect source, empty when the source was accepted.
    registers: Vec<Register>,
    /// Declared-vs-defined disagreements, across domains.
    mismatches: Vec<dialect::OpMismatch>,
}

impl Verdicts {
    fn is_empty(&self) -> bool {
        self.registers.is_empty() && self.mismatches.is_empty()
    }

    fn refused(&self) -> usize {
        self.registers.iter().filter(|r| !r.is_empty()).count()
    }

    /// Anything at all that should stop a build.
    fn ok(&self) -> bool {
        self.refused() == 0 && self.mismatches.is_empty()
    }
}

/// The closing line a person reads.
fn summary(found: &Verdicts) -> String {
    let refused = found.refused();
    let findings: usize = found
        .registers
        .iter()
        .filter(|r| !r.is_empty())
        .map(|r| r.rejections.len())
        .sum();
    let mut line = format!(
        "{} source(s) checked, {} accepted, {} refused{}",
        found.registers.len(),
        found.registers.len() - refused,
        refused,
        match findings {
            0 => String::new(),
            n => format!(" ({n} finding(s))"),
        }
    );
    if !found.mismatches.is_empty() {
        line.push_str(&format!(
            "; {} declared/defined mismatch(es)",
            found.mismatches.len()
        ));
    }
    line
}

/// The whole run as one JSON document.
fn envelope(root: &Path, found: &Verdicts) -> String {
    let doc = serde_json::json!({
        "schema_version": CHECK_SCHEMA,
        "root": root.display().to_string(),
        "accepted": found.registers.iter().filter(|r| r.is_empty()).count(),
        "refused": found.refused(),
        "sources": found.registers.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
        // Additive: a parser pinned to forge-check/1 ignores a key it does not
        // know, and this one is empty on every workspace that has no
        // disagreement.
        "op_mismatches": found.mismatches.iter().map(|m| serde_json::json!({
            "domain": m.domain,
            "op": m.op,
            "kind": match m.kind {
                dialect::MismatchKind::DeclaredNotDefined => "declared_not_defined",
                dialect::MismatchKind::DefinedNotDeclared => "defined_not_declared",
            },
            "message": m.render(),
        })).collect::<Vec<_>>(),
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
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        let registers = &found.registers;
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
            summary(&found),
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
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        let doc: serde_json::Value = serde_json::from_str(&envelope(ws.path(), &found)).unwrap();
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
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        let registers = &found.registers;
        assert_eq!(registers.len(), 1);
        assert!(
            registers[0].is_empty(),
            "a domain reading another domain's declared table must be accepted:\n{}",
            registers[0].render()
        );
    }
    /// A domain can declare an op it never defines, or define one it never
    /// declares, and until this nothing said so.
    ///
    /// Both directions are real and they fail differently. Declared-not-
    /// defined puts a route in the contract that 404s when anyone calls it.
    /// Defined-not-declared is worse in a quieter way: the op compiles,
    /// deploys, and has no contract — so it is unreachable through the API and
    /// absent from the generated SDK. Code that runs and cannot be called.
    #[test]
    fn a_domain_that_declares_and_defines_different_ops_is_refused() {
        const HELLO: &str = r#"from dataclasses import dataclass

from forge import OpContext, Value, op


@dataclass
class Greeting:
    text: str


@op("hello")
def hello(ctx: OpContext, input: Value) -> Greeting:
    return Greeting(text="hi")
"#;
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/a/services/hello.py", HELLO),
            // declares one op that exists nowhere, and omits the one that does
            (
                "domains/a/service.json",
                r#"{"name":"a","domain":"a","operations":[{"name":"ghost"}]}"#,
            ),
        ]);
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        assert!(
            found.registers.iter().all(|r| r.is_empty()),
            "sources are fine"
        );
        assert!(!found.ok(), "a mismatch must stop the build");
        let rendered: Vec<String> = found.mismatches.iter().map(|m| m.render()).collect();
        assert_eq!(rendered.len(), 2, "{rendered:?}");
        assert!(
            rendered.iter().any(|r| r.contains("declares `ghost`")),
            "{rendered:?}"
        );
        assert!(
            rendered.iter().any(|r| r.contains("`hello` is defined")),
            "{rendered:?}"
        );
    }

    /// A domain whose declarations match is silent, and a domain with no
    /// service.json at all is silent — "no declaration file" is not "declares
    /// nothing", and treating it as the latter would refuse every workspace
    /// mid-authoring.
    #[test]
    fn a_matching_domain_and_an_undeclared_one_are_both_quiet() {
        const HELLO: &str = r#"from dataclasses import dataclass

from forge import OpContext, Value, op


@dataclass
class Greeting:
    text: str


@op("hello")
def hello(ctx: OpContext, input: Value) -> Greeting:
    return Greeting(text="hi")
"#;
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/a/services/hello.py", HELLO),
            (
                "domains/a/service.json",
                r#"{"name":"a","domain":"a","operations":[{"name":"hello"}]}"#,
            ),
            // b has sources and no service.json — mid-authoring, not an error
            ("domains/b/services/hello.py", HELLO),
        ]);
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        assert!(found.mismatches.is_empty(), "{:?}", found.mismatches);
        assert!(found.ok());
    }

    /// A refused source must not also be accused of not defining its ops.
    ///
    /// The front end returns no ops for a source it rejected, so every op that
    /// source would have defined looks "declared and not defined" — a second,
    /// wrong diagnosis stacked on the real one, pointing at the wrong file.
    #[test]
    fn a_refused_source_suppresses_the_mismatch_report() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/b/services/hello.py", REFUSED),
            (
                "domains/b/service.json",
                r#"{"name":"b","domain":"b","operations":[{"name":"hello"}]}"#,
            ),
        ]);
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(found.refused(), 1);
        assert!(
            found.mismatches.is_empty(),
            "the refusal is the diagnosis; do not stack a wrong one on it: {:?}",
            found.mismatches
        );
    }
}
