//! `forge test` — a domain's tests, in the op's own language, run by the
//! interpreter the platform serves (forge-lang CF-23).
//!
//! A test file lives at `domains/<d>/tests/`, beside `services/`: a
//! pytest-style `def test_*()` in Python, `describe`/`it` in TypeScript,
//! `@Test` methods in Java and `#[test] fn` in Rust, each written against one
//! fixture surface (`input`, `seed`, `http`, `call`, `secret`, `user`, then
//! `run` or `run_error`) and the lane's own equality assertion. The library
//! half is `forge_lang_test`, which this command and later the platform
//! share; this file finds the workspace's domains, hands each one over with
//! the schema and contracts `forge check` reads, and reports.
//!
//! No build runs: the domain's ops are checked as the push path checks them
//! and served from the bundle the engine would load, against an in-memory
//! store over the seeded rows and the declared crossing answers. Test files
//! never enter what a push serves — `dialect::sources` and the control
//! plane's walk read `services/` only, and a test below holds that.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use forge_lang_test::{DomainUnderTest, Outcome, Status, envelope, human, run_domain, test_files};

use crate::dialect;

#[derive(Debug, Args)]
pub struct TestArgs {
    /// Workspace root (the directory holding `workspace.json`). Defaults to
    /// the current directory.
    #[arg(long)]
    manifest_dir: Option<PathBuf>,

    /// Run one domain's tests rather than every domain's.
    #[arg(long)]
    domain: Option<String>,

    /// Print the `forge-test/1` envelope on stdout instead of the report.
    #[arg(long)]
    json: bool,

    /// Run only the tests whose name contains this text.
    filter: Option<String>,
}

pub fn run(args: TestArgs) -> Result<()> {
    let root = args.manifest_dir.unwrap_or_else(|| PathBuf::from("."));
    let outcomes = match collect(&root, args.domain.as_deref(), args.filter.as_deref()) {
        Ok(o) => o,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };
    if args.json {
        println!("{}", envelope(&outcomes));
    } else if outcomes.is_empty() {
        eprintln!(
            "no tests under {}/domains/*/tests/ — nothing to run",
            root.display()
        );
    } else {
        eprint!("{}", human(&outcomes));
    }
    if !all_passed(&outcomes) {
        std::process::exit(1);
    }
    Ok(())
}

/// Whether the run exits zero: every test passed and nothing was refused.
pub(crate) fn all_passed(outcomes: &[Outcome]) -> bool {
    outcomes.iter().all(|o| o.status == Status::Pass)
}

/// Every test of the workspace (or of one domain), run.
///
/// `Err` is this machine's problem — no workspace, an unreadable schema —
/// and never a test's: a refused or failing test is an [`Outcome`].
pub(crate) fn collect(
    root: &Path,
    domain: Option<&str>,
    filter: Option<&str>,
) -> Result<Vec<Outcome>, String> {
    if !root.join("workspace.json").exists() {
        return Err(format!(
            "no workspace.json at {} — `forge test` reads a workspace; pass \
             --manifest-dir or run from the workspace root",
            root.display()
        ));
    }
    let domains: Vec<PathBuf> = dialect::domains(root)
        .into_iter()
        .filter(|d| domain.is_none_or(|name| dialect::domain_name(d) == name))
        .collect();
    if let (Some(name), true) = (domain, domains.is_empty()) {
        return Err(format!(
            "no domain `{name}` under {}/domains/",
            root.display()
        ));
    }
    let (tables, _) = dialect::workspace_tables(root)?;
    let contracts = dialect::contract_surfaces(root)?;
    let mut outcomes = Vec::new();
    for d in domains {
        let tests = test_files(&d);
        if tests.is_empty() {
            continue;
        }
        let inputs = dialect::input_schemas(&d).map_err(|e| e.to_string())?;
        outcomes.extend(run_domain(
            &DomainUnderTest {
                name: dialect::domain_name(&d),
                sources: dialect::sources_of(&d),
                tests,
                tables: tables.as_ref(),
                contracts: &contracts,
                inputs,
            },
            filter,
        ));
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OP: &str = r#"from dataclasses import dataclass

from forge import OpContext, Value, http, op, storage


@dataclass
class Total:
    total: int
    status: str


@op("invoice_total")
def invoice_total(ctx: OpContext, input: Value) -> Total:
    customer: str = input.get("customer", "")
    resp: Value = storage.query(
        {
            "from": "invoices",
            "filter": {"column": "customer", "op": "eq", "value": customer},
            "limit": 100,
        }
    )
    rows: list[Value] = storage.rows(resp)
    total: int = 0
    for row in rows:
        total = total + row.get("amount", 0)
    answered: Value = http.fetch("GET", "https://fx.example.com/v1/usd", [], None)
    return Total(total=total, status=answered.get("status", ""))
"#;

    const TESTS: &str = r#"from forge.testing import fixture


def test_sums_the_seeded_rows():
    t = fixture("invoice_total")
    t.input({"customer": "acme"})
    t.seed("invoices", [{"customer": "acme", "amount": 5}, {"customer": "acme", "amount": 7}])
    t.http("GET", "https://fx.example.com/v1/usd", {"body": "1.1"})
    out = t.run()
    assert out == {"total": 12, "status": "ok"}


def test_expects_the_wrong_sum():
    t = fixture("invoice_total")
    t.input({"customer": "acme"})
    t.seed("invoices", [{"customer": "acme", "amount": 5}])
    t.http("GET", "https://fx.example.com/v1/usd", {"body": "1.1"})
    out = t.run()
    assert out["total"] == 6


def test_forgets_the_http_answer():
    t = fixture("invoice_total")
    out = t.run()
    assert out["total"] == 0
"#;

    const SCHEMA: &str = r#"{"name": "invoices", "archetype": "Base", "label": "Invoice",
"plural_label": "Invoices", "columns": [
{"name": "customer", "type": "string", "required": true, "label": "Customer"},
{"name": "amount", "type": "integer", "required": true, "label": "Amount"}]}"#;

    fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, body) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        dir
    }

    fn billing() -> tempfile::TempDir {
        workspace(&[
            ("workspace.json", "{}"),
            ("domains/billing/schemas/invoices.table.json", SCHEMA),
            ("domains/billing/services/invoice_total.py", OP),
            ("domains/billing/tests/test_billing.py", TESTS),
        ])
    }

    fn named<'a>(outcomes: &'a [Outcome], name: &str) -> &'a Outcome {
        outcomes.iter().find(|o| o.name == name).unwrap()
    }

    #[test]
    fn a_domains_tests_pass_and_fail_with_the_failing_assertion() {
        let ws = billing();
        let outcomes = collect(ws.path(), None, None).unwrap();
        assert_eq!(outcomes.len(), 3, "{outcomes:#?}");
        assert_eq!(
            named(&outcomes, "test_sums_the_seeded_rows").status,
            Status::Pass
        );
        let wrong = named(&outcomes, "test_expects_the_wrong_sum");
        assert_eq!(wrong.status, Status::Fail);
        let f = wrong.failure.as_ref().unwrap();
        assert_eq!(f.kind, "assertion");
        assert_eq!(f.message, "result[\"total\"]: expected 6, got 5");
        assert_eq!(f.line, Some(19));
        assert!(!all_passed(&outcomes));
        let report = human(&outcomes);
        assert!(report.contains("FAIL    billing"), "{report}");
        assert!(
            report.contains("test_billing.py:19 [assertion]"),
            "{report}"
        );
    }

    #[test]
    fn an_undeclared_crossing_fails_naming_it() {
        let ws = billing();
        let outcomes = collect(ws.path(), None, Some("forgets")).unwrap();
        assert_eq!(outcomes.len(), 1);
        let f = outcomes[0].failure.as_ref().unwrap();
        assert_eq!(f.kind, "undeclared_crossing");
        assert!(
            f.message
                .contains("http `GET https://fx.example.com/v1/usd`"),
            "{}",
            f.message
        );
    }

    /// The `--json` envelope `forge test` prints, field for field.
    #[test]
    fn the_json_envelope_is_stable() {
        let ws = billing();
        let doc = envelope(&collect(ws.path(), Some("billing"), None).unwrap());
        assert_eq!(doc["schema"], "forge-test/1");
        assert_eq!(
            doc["totals"],
            serde_json::json!({"passed": 1, "failed": 2, "refused": 0, "total": 3})
        );
        let first = &doc["tests"][0];
        let keys: Vec<&str> = first
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["domain", "failure", "file", "name", "status"]);
        let wrong = doc["tests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "test_expects_the_wrong_sum")
            .unwrap();
        let fkeys: Vec<&str> = wrong["failure"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(fkeys, ["kind", "line", "message"]);
    }

    #[test]
    fn an_unknown_domain_is_the_callers_error() {
        let ws = billing();
        let err = collect(ws.path(), Some("nope"), None).unwrap_err();
        assert!(err.contains("no domain `nope`"), "{err}");
    }

    /// A test file is never part of what a push checks or serves: the file
    /// below would be REFUSED as an op (it declares none and imports
    /// `forge.testing`), and `forge check` accepts the workspace and names
    /// only the op, because the enumeration is `services/` alone.
    #[test]
    fn test_files_never_enter_the_checked_sources() {
        let ws = billing();
        let sources = dialect::sources(ws.path());
        assert_eq!(sources.len(), 1);
        assert!(sources[0].ends_with("domains/billing/services/invoice_total.py"));
        let refused = forge_lang_rustgen::check_only_with(
            &ws.path().join("domains/billing/tests/test_billing.py"),
            None,
            forge_lang_rustgen::NativeValidation::Skip,
            &Default::default(),
        );
        assert!(refused.is_err(), "the op checker accepted a test file");
        let found = match crate::cmd::check::check(ws.path()) {
            Ok(v) => v,
            Err(e) if e.contains("python3") || e.contains("CPython") => {
                eprintln!("skipping the `forge check` half: {e}");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        assert!(found.ok(), "forge check refused the workspace");
        assert_eq!(found.registers.len(), 1);
        assert!(
            found.registers.iter().all(|r| !r.path.contains("/tests/")),
            "forge check named a test file"
        );
    }
}
