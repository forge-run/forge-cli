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
use forge_lang_rustgen::{Card, CompileError, Register, check_only};

use crate::dialect;

/// The versioned envelope `--format json` prints.
///
/// Each element of `sources` is one `forge-lang-diagnostics/2` object exactly
/// as `forge-lang --format json` emits it — same fields, same meanings — so a
/// parser written against that contract reads them unchanged. This wrapper
/// exists only because a workspace has many sources and that envelope has one
/// `path`. Additive-only, and the `N` bumps when a field is added
/// (`forge-lang/REJECTIONS.md`, the machine-envelope section).
const CHECK_SCHEMA: &str = "forge-check/2";

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
        eprintln!("{}", human_report(&found));
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
pub(crate) fn check(root: &Path) -> Result<Verdicts, String> {
    if !root.join("workspace.json").exists() {
        return Err(format!(
            "no workspace.json at {} — `forge check` reads a workspace; pass \
             --manifest-dir or run from the workspace root",
            root.display()
        ));
    }
    // Pinned units (shared-code SC-11): a tree with `forge.units.json` is
    // checked over a scratch copy with each pin staged beside its consumers,
    // exactly as the control plane splices them at push, and every verdict
    // is spelled back under the workspace root. A tree with no pins is read
    // in place, as it always was.
    let pinned = match crate::pins::read(root)? {
        Some(pins) => {
            let resolved = crate::pins::resolve(root, &pins)?;
            Some(crate::pins::stage(root, &resolved)?)
        }
        None => None,
    };
    let source_root: &Path = pinned.as_ref().map_or(root, |s| s.root.as_path());
    let sources = dialect::sources(source_root);
    if sources.is_empty() {
        return Ok(Verdicts::default());
    }
    // A sibling unit is in that enumeration (shared-code SC-8): a Python,
    // TypeScript or Rust unit sits beside the op that imports it. The
    // toolchain's `partition` is the one answer this command, the emit and
    // the server's push-time checker share, so the three cannot disagree
    // about which file is an op; a unit is checked through its consumer and
    // gets no verdict of its own.
    let forge_lang_rustgen::partition::Partition {
        ops: sources,
        units,
    } = forge_lang_rustgen::partition::partition(&sources);

    // Snapshot-first: a workspace with a committed `schema.lock` is judged
    // against the compiled schema the converge applies — system fields and
    // the runtime-owned set included — after a staleness check. A workspace
    // without one keeps the directory walk; rule zero stays off when a
    // workspace declares nothing at all.
    let (tables, schema) = dialect::workspace_tables(root)?;

    // Every source is checked. Stopping at the first refusal would make a
    // second run necessary to see the second problem, which is the round trip
    // this command exists to remove.
    let mut registers: Vec<Register> = Vec::with_capacity(sources.len());
    // The EV-7 cost cards, one entry per source aligned to `registers` — the
    // accepted source's `Accepted::cost` carried through unchanged, and an
    // empty vec for a refused one (a refusal has no card). Reused, never
    // recomputed: the front end walked the tree once and this is that result.
    let mut cards: Vec<Vec<(String, Card)>> = Vec::with_capacity(sources.len());
    // Op names per domain, collected from the SAME front-end pass that
    // produces the verdicts — a second pass would be a second chance to
    // disagree about the same file.
    let mut defined: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for source in &sources {
        match check_only(source, tables.as_ref()) {
            // The emit refuses an op-less source, and this command says the
            // same thing in the same words rather than accepting a file
            // `forge wasm-build` will refuse a minute later.
            Ok(accepted) if accepted.ops.is_empty() => {
                return Err(forge_lang_rustgen::partition::no_ops_text(&accepted.source)
                    .trim_end()
                    .to_string());
            }
            Ok(accepted) => {
                if let Some(domain) = domain_of(source_root, source) {
                    defined.entry(domain).or_default().extend(accepted.ops);
                }
                cards.push(accepted.cost);
                registers.push(Register::new(accepted.source));
            }
            Err(CompileError::Rejected(register)) => {
                registers.push(register);
                cards.push(Vec::new());
            }
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
    let (registers, units) = match &pinned {
        Some(staged) => (
            registers
                .into_iter()
                .map(|mut r| {
                    r.path = crate::pins::unstage(&r.path, &staged.root, root);
                    r
                })
                .collect(),
            units
                .iter()
                .map(|u| {
                    PathBuf::from(crate::pins::unstage(
                        &u.display().to_string(),
                        &staged.root,
                        root,
                    ))
                })
                .collect(),
        ),
        None => (registers, units),
    };
    Ok(Verdicts {
        registers,
        cards,
        mismatches,
        schema,
        units,
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
pub(crate) struct Verdicts {
    /// One per dialect source, empty when the source was accepted.
    pub(crate) registers: Vec<Register>,
    /// The EV-7 cost cards, aligned to `registers`: `Accepted::cost` for an
    /// accepted source, an empty vec for a refused one. Each inner entry is
    /// `(op, card)` in the source's declaration order.
    pub(crate) cards: Vec<Vec<(String, Card)>>,
    /// Declared-vs-defined disagreements, across domains.
    mismatches: Vec<dialect::OpMismatch>,
    /// The compiled snapshot's content hash when the workspace was judged
    /// against a `schema.lock` — named in the output so a verdict can be
    /// tied to the exact schema it was made under. `None` on the
    /// directory-walk path.
    schema: Option<String>,
    /// The units the sources reached (shared-code SC-8), in tree order —
    /// files the enumeration listed and the partition set aside. Named in
    /// the output so a reader can see a unit was resolved rather than
    /// silently skipped.
    units: Vec<PathBuf>,
}

impl Verdicts {
    fn is_empty(&self) -> bool {
        self.registers.is_empty() && self.mismatches.is_empty()
    }

    fn refused(&self) -> usize {
        self.registers.iter().filter(|r| !r.is_empty()).count()
    }

    /// Anything at all that should stop a build.
    pub(crate) fn ok(&self) -> bool {
        self.refused() == 0 && self.mismatches.is_empty()
    }
}

/// The engineer-facing cost card for one op, formatted in ONE place so the
/// `forge check` and `forge push` human output cannot drift. The exact shape
/// is `<op>: reads N (max M rows) writes W escaping E`, where the numbers are
/// the EV-7 card's `reads`, `max_rows`, `writes` and `escaping`.
pub(crate) fn card_line(op: &str, card: &Card) -> String {
    format!(
        "{op}: reads {} (max {} rows) writes {} escaping {}",
        card.reads, card.max_rows, card.writes, card.escaping
    )
}

/// The whole human run as one string: per-source acceptance and its cost
/// cards, the refusals verbatim, the declared/defined mismatches, then the
/// summary. An accepted source prints one [`card_line`] per op after its
/// acceptance line; a refused source prints its register and no card, exactly
/// as before.
fn human_report(found: &Verdicts) -> String {
    let mut out = String::new();
    for (register, cards) in found.registers.iter().zip(&found.cards) {
        if register.is_empty() {
            out.push_str(&format!("{}: accepted\n", register.path));
            for (op, card) in cards {
                out.push_str(&card_line(op, card));
                out.push('\n');
            }
        } else {
            // `render` is already newline-terminated.
            out.push_str(&register.render());
        }
    }
    for m in &found.mismatches {
        out.push_str(&format!("error: {}\n", m.render()));
    }
    out.push_str(&summary(found));
    out
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
    if !found.units.is_empty() {
        line.push_str(&format!("; {} unit(s) resolved", found.units.len()));
    }
    if let Some(hash) = &found.schema {
        line.push_str(&format!(" — judged against schema.lock {hash}"));
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
        // forge-check/2: which schema the verdicts were made under — the
        // compiled snapshot's content hash, or null on the directory walk.
        "schema_lock": found.schema,
        // forge-check/2, additive: the units the sources reached, so a
        // machine reader sees the file was resolved and not skipped.
        "units": found.units.iter().map(|u| u.display().to_string()).collect::<Vec<_>>(),
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

    /// The same accepted op in the OTHER dialect spelling.
    const ACCEPTED_TS: &str = r#"import { op, OpContext, Value } from "forge";

interface Greeting {
  text: string;
}

export const hello = op("hello", (ctx: OpContext, input: Value) => {
  const g: Greeting = { text: "hi" };
  return g;
});
"#;

    /// `var` is FL1010 — outside the subset, and in the FL1000 band, which is
    /// the half of this fixture that matters. See the test below.
    const REFUSED_TS: &str = r#"import { op, OpContext, Value } from "forge";

interface Greeting {
  text: string;
}

export const hello = op("hello", (ctx: OpContext, input: Value) => {
  var g: Greeting = { text: "hi" };
  return g;
});
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

    /// A `.ts` source is read by the TS front half, and the register says so.
    ///
    /// Until P7.5-builders the pipeline ran the Python front end
    /// unconditionally, so every `.ts` source in a workspace came back FL0001
    /// SyntaxError — CPython reporting a grammar the file was never written
    /// in. The CODE BAND is what lets this test assert which half ran rather
    /// than merely that something passed: TS refusals are FL1xxx and the
    /// Python half cannot emit one, so pinning the code pins the dispatch.
    ///
    /// No `skip_without_cpython` guard, and its absence is deliberate: the TS
    /// front half runs no reference toolchain, so this holds on a machine
    /// with no python3 at all.
    #[test]
    fn a_ts_source_is_checked_by_the_ts_front_half() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/a/services/hello.ts", ACCEPTED_TS),
            ("domains/b/services/hello.ts", REFUSED_TS),
        ]);
        let found = check(ws.path()).unwrap_or_else(|e| panic!("{e}"));
        let registers = &found.registers;
        assert_eq!(registers.len(), 2);
        assert!(
            registers[0].is_empty(),
            "domains/a should be accepted:\n{}",
            registers[0].render()
        );
        let rendered = registers[1].render();
        assert!(rendered.contains("FL1010"), "{rendered}");
        assert!(
            !rendered.contains("FL0001"),
            "FL0001 is CPython's parse error, so the PYTHON front half read a \
             .ts file:\n{rendered}"
        );
        assert_eq!(
            summary(&found),
            "2 source(s) checked, 1 accepted, 1 refused (1 finding(s))"
        );
    }

    /// Both spellings in one workspace, checked in one pass.
    #[test]
    fn a_workspace_can_hold_both_spellings_at_once() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/a/services/hello.py", ACCEPTED),
            ("domains/b/services/hello.ts", ACCEPTED_TS),
        ]);
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(found.registers.len(), 2);
        assert!(found.ok(), "{}", summary(&found));
        assert_eq!(
            summary(&found),
            // No findings clause: `summary` omits it when there are none.
            "2 source(s) checked, 2 accepted, 0 refused"
        );
    }

    /// The machine surface: one `forge-check/1` wrapper whose `sources` are
    /// the `forge-lang-diagnostics/2` objects a parser already knows.
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
        assert_eq!(source["schema_version"], "forge-lang-diagnostics/2");
        assert_eq!(source["diagnostics"][0]["code"], "FL0024");
        assert_eq!(source["diagnostics"][0]["severity"], "error");
    }

    /// The editor's schema types are never an input to the verdict
    /// (typed-boundary TB-6): the same workspace, judged against the same
    /// `schema.lock`, answers byte-for-byte the same envelope with
    /// `.forge/types/` absent and present — and the tree holds dozens of `.py`
    /// and `.ts` files a checker that walked it would have judged.
    #[test]
    fn the_editor_types_are_never_an_input_to_the_verdict() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/a/services/hello.py", ACCEPTED),
            ("domains/b/services/hello.py", REFUSED),
            ("domains/c/services/hello.ts", ACCEPTED_TS),
        ]);
        let compiled = crate::cmd::schema::compile_snapshot(ws.path()).unwrap();
        std::fs::write(
            ws.path().join(forge_lang_rustgen::SNAPSHOT_FILE),
            &compiled.text,
        )
        .unwrap();
        let judge = || match check(ws.path()) {
            Ok(v) => Some(envelope(ws.path(), &v)),
            Err(e) if skip_without_cpython(&e) => None,
            Err(e) => panic!("{e}"),
        };
        let Some(absent) = judge() else { return };
        crate::cmd::schema::refresh_types(ws.path());
        let types = ws
            .path()
            .join(forge_lang_rustgen::workspace_types::TYPES_DIR);
        assert!(types.join("py/forge_schema/audit_events.py").is_file());
        assert!(types.join("ts/schema.ts").is_file());
        let present = judge().expect("the toolchain answered once already");
        assert_eq!(absent, present);
        let doc: serde_json::Value = serde_json::from_str(&present).unwrap();
        assert_eq!(doc["sources"].as_array().map(Vec::len), Some(3));
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

    /// `card_line` is the anti-drift seam, so its exact string is pinned here.
    #[test]
    fn card_line_pins_the_exact_format() {
        let card = Card {
            reads: 2,
            writes: 1,
            escaping: 3,
            max_rows: 50,
        };
        assert_eq!(
            card_line("hello", &card),
            "hello: reads 2 (max 50 rows) writes 1 escaping 3"
        );
    }

    /// An accepted source's human output carries one cost line per op. The
    /// `ACCEPTED` fixture makes no crossings, so its card is all zeros — a
    /// card whose exact value can be stated.
    #[test]
    fn the_human_output_carries_the_cost_card_for_an_accepted_op() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/a/services/hello.py", ACCEPTED),
        ]);
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        let report = human_report(&found);
        assert!(
            report.contains("hello: reads 0 (max 0 rows) writes 0 escaping 0"),
            "{report}"
        );
    }

    /// FL0090 — a `storage.query` inside a `for` — is refused, so its human
    /// output prints the register's alternative and NO card line.
    #[test]
    fn a_refused_fl0090_op_prints_the_alternative_and_no_card() {
        const QUERY_IN_LOOP: &str = r#"from dataclasses import dataclass

from forge import OpContext, Value, op, storage


@dataclass
class Counts:
    seen: int


@op("per_element")
def per_element(ctx: OpContext, input: Value) -> Counts:
    ids: list[Value] = input.get("ids", [])
    seen: int = 0
    for entry in ids:
        wid: str = entry.get("id", "")
        resp: Value = storage.query(
            {
                "from": "_agent_runs",
                "filter": {"column": "workspace_id", "op": "eq", "value": wid},
                "limit": 50,
            }
        )
        seen = seen + len(storage.rows(resp))
    return Counts(seen=seen)
"#;
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/a/services/loop.py", QUERY_IN_LOOP),
        ]);
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(found.refused(), 1, "the loop read must be refused");
        let report = human_report(&found);
        assert!(report.contains("FL0090"), "{report}");
        // The refusal names its alternative — the batched `query_batch` shape.
        assert!(report.contains("storage.query_batch"), "{report}");
        // A refused source has no card, so the card-line signature is absent.
        assert!(
            !report.contains("rows) writes"),
            "a refused source must print no card line:\n{report}"
        );
    }

    /// A Python unit beside the op that imports it (shared-code SC-8).
    const CONSUMER_WITH_UNIT: &str = r#"from dataclasses import dataclass

from forge import OpContext, Value, op
from plan_rules import tier_for


@dataclass
class Badge:
    tier: str


@op("plan_badge")
def plan_badge(ctx: OpContext, input: Value) -> Badge:
    return Badge(tier=tier_for("pro"))
"#;

    const UNIT: &str = r#"def tier_for(name: str) -> str:
    if name == "pro":
        return "pro"
    return "free"
"#;

    /// The enumeration lists the unit beside its consumer, and the verdicts
    /// do not: a unit is checked through the op that imports it and named
    /// as resolved, never counted as a source or refused as op-less.
    #[test]
    fn a_unit_beside_its_consumer_is_resolved_and_gets_no_verdict_of_its_own() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/plans/services/plan_badge.py", CONSUMER_WITH_UNIT),
            ("domains/plans/services/plan_rules.py", UNIT),
        ]);
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(found.registers.len(), 1, "{}", human_report(&found));
        assert!(found.ok(), "{}", human_report(&found));
        assert_eq!(found.units.len(), 1);
        assert!(found.units[0].ends_with("domains/plans/services/plan_rules.py"));
        assert_eq!(
            summary(&found),
            "1 source(s) checked, 1 accepted, 0 refused; 1 unit(s) resolved"
        );
        let doc: serde_json::Value = serde_json::from_str(&envelope(ws.path(), &found)).unwrap();
        assert_eq!(doc["sources"].as_array().unwrap().len(), 1);
        assert!(doc["units"][0].as_str().unwrap().ends_with("plan_rules.py"));
    }

    /// A unit importing a unit is refused against the unit's own file, with
    /// the one-level code — the same register the server renders at push.
    #[test]
    fn a_two_level_import_is_refused_against_the_middle_unit() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/plans/services/plan_badge.py", CONSUMER_WITH_UNIT),
            (
                "domains/plans/services/plan_rules.py",
                "from tier_names import name_of\n\n\ndef tier_for(name: str) -> str:\n    return name_of(name)\n",
            ),
            (
                "domains/plans/services/tier_names.py",
                "def name_of(name: str) -> str:\n    return name\n",
            ),
        ]);
        let found = match check(ws.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(found.registers.len(), 1, "{}", human_report(&found));
        let rendered = found.registers[0].render();
        assert!(rendered.contains("FL0115"), "{rendered}");
        assert!(rendered.contains("plan_rules.py"), "{rendered}");
        assert_eq!(found.units.len(), 2, "both files are units of the one op");
    }

    /// A helper-only file no op imports is the op-less source the emit
    /// refuses, in the emit's own words.
    #[test]
    fn an_orphan_helper_file_is_refused_in_the_emits_words() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/plans/services/plan_badge.py", ACCEPTED),
            ("domains/plans/services/helpers.py", UNIT),
        ]);
        let err = match check(ws.path()) {
            Ok(v) => panic!("accepted an op-less source:\n{}", human_report(&v)),
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => e,
        };
        let expected = forge_lang_rustgen::partition::no_ops_text(
            &ws.path()
                .join("domains/plans/services/helpers.py")
                .display()
                .to_string(),
        );
        assert_eq!(err, expected.trim_end());
    }

    // ── pinned units (shared-code SC-11) ────────────────────────────

    const STORAGE_UNIT: &str = "from forge import Value, storage\n\n\ndef tier_for(name: str) -> str:\n    resp: Value = storage.query({\"from\": \"plans\", \"limit\": 10})\n    return \"pro\"\n";

    /// Put `source` in the workspace's unit store under its own address and
    /// pin `name` to it, as `forge units pull` would.
    fn pin_into_store(ws: &Path, name: &str, file_name: &str, source: &str) -> String {
        let address = crate::pins::unit_address(source.as_bytes());
        let dir = crate::pins::store_dir(ws).join(address.as_str());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file_name), source).unwrap();
        std::fs::write(
            ws.join(forge_platform_wire::UNIT_PINS_PATH),
            format!(r#"{{"version":1,"units":{{"{name}":"{address}"}}}}"#),
        )
        .unwrap();
        address.to_string()
    }

    /// The CLI half of the pair: the same storage-reaching unit is accepted
    /// carried beside the op and refused pinned, with `FL0117` against the
    /// unit's file spelled under the WORKSPACE root, naming its address —
    /// the register the control plane renders at push over the same tree.
    #[test]
    fn a_pinned_unit_reaching_storage_is_refused_and_the_same_unit_in_tree_is_not() {
        let carried = workspace(&[
            ("workspace.json", "{}"),
            ("domains/plans/services/plan_badge.py", CONSUMER_WITH_UNIT),
            ("domains/plans/services/plan_rules.py", STORAGE_UNIT),
        ]);
        let found = match check(carried.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        assert!(
            found.ok(),
            "the in-tree arm is refused: {}",
            human_report(&found)
        );

        let pinned = workspace(&[
            ("workspace.json", "{}"),
            ("domains/plans/services/plan_badge.py", CONSUMER_WITH_UNIT),
        ]);
        let address = pin_into_store(pinned.path(), "plan_rules", "plan_rules.py", STORAGE_UNIT);
        let found = match check(pinned.path()) {
            Ok(v) => v,
            Err(e) if skip_without_cpython(&e) => return,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(found.registers.len(), 1);
        let register = &found.registers[0];
        assert_eq!(register.rejections.len(), 1, "{}", human_report(&found));
        assert_eq!(register.rejections[0].code, "FL0117");
        assert!(
            register
                .path
                .starts_with(&pinned.path().display().to_string()),
            "spelled under the workspace root, not the scratch copy: {}",
            register.path
        );
        assert!(
            register
                .path
                .ends_with("domains/plans/services/plan_rules.py")
        );
        let text = register.rejections[0].to_string();
        assert!(text.contains(&address), "{text}");
        assert!(
            !pinned
                .path()
                .join("domains/plans/services/plan_rules.py")
                .exists(),
            "the pinned unit is never written into the customer's tree"
        );
        assert_eq!(found.units.len(), 1);
        assert!(found.units[0].starts_with(pinned.path()));
    }

    /// A pin the store does not hold refuses before any source is read,
    /// naming the unit, the address and the verb that fetches it.
    #[test]
    fn a_pin_missing_from_the_store_is_refused_naming_it() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/plans/services/plan_badge.py", CONSUMER_WITH_UNIT),
            (
                forge_platform_wire::UNIT_PINS_PATH,
                r#"{"version":1,"units":{"plan_rules":"sha256-e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}}"#,
            ),
        ]);
        let err = check(ws.path()).expect_err("an unresolvable pin");
        assert!(err.contains("`plan_rules`"), "{err}");
        assert!(err.contains("sha256-e3b0c442"), "{err}");
        assert!(err.contains("forge units pull"), "{err}");
    }

    /// Bytes in the store that do not hash to their address are refused:
    /// the store is verified, never trusted.
    #[test]
    fn a_tampered_store_entry_is_refused() {
        let ws = workspace(&[
            ("workspace.json", "{}"),
            ("domains/plans/services/plan_badge.py", CONSUMER_WITH_UNIT),
        ]);
        let address = pin_into_store(ws.path(), "plan_rules", "plan_rules.py", UNIT);
        std::fs::write(
            crate::pins::store_dir(ws.path())
                .join(&address)
                .join("plan_rules.py"),
            STORAGE_UNIT,
        )
        .unwrap();
        let err = check(ws.path()).expect_err("edited bytes");
        assert!(err.contains("does not hash to its address"), "{err}");
    }
}
