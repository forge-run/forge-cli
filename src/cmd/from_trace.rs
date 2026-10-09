//! `forge test --from-trace <id|file>` — a production failure as a test, in
//! the op's own language (forge-lang OR-3).
//!
//! The capture is one of the workspace's OR-1 production captures: fetched
//! by id from `GET /api/v1/manage/captures/<id>` (admin-tier, read under the
//! caller's own context), or read from a file holding one capture as that
//! route serves it. The library half, `forge_lang_test::from_trace`, writes
//! the test — input, the rows the op read as `seed`, every crossing's answer,
//! the captured instant as `now`, and the redactions named in a header — into
//! `domains/<d>/tests/` of the domain whose `services/` define the op, in
//! that source's lane. Then the test runs once, so the report says whether
//! it fails with the captured error: the loop an agent runs is capture ->
//! test (red) -> fix -> `forge test` (green), with nothing read off a log.
//!
//! Exit 0: the test is written and fails with the captured error. Exit 1:
//! written, but it does not reproduce the failure (the report says why).
//! Exit 2: no test written.

use std::path::{Path, PathBuf};

use anyhow::Result;
use forge_lang_test::from_trace::{self, Generated, Reproduction};
use forge_lang_test::{DomainUnderTest, bundle, test_files};
use forge_platform_wire::OpCapture;

use crate::client::ForgeClient;
use crate::dialect;

/// Where the capture comes from: a file on this machine, or the workspace.
pub(crate) fn is_file(source: &str) -> bool {
    Path::new(source).is_file()
}

/// The capture, from a file or by id from the workspace's manage route.
pub(crate) async fn load(source: &str, client: Option<&ForgeClient>) -> Result<OpCapture, String> {
    if is_file(source) {
        let text = std::fs::read_to_string(source).map_err(|e| format!("{source}: {e}"))?;
        return serde_json::from_str(&text)
            .map_err(|e| format!("{source} is not one production capture ({e})"));
    }
    if source.is_empty()
        || !source
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "`{source}` is neither a capture file nor a capture id (letters, digits, `-`, `_`)"
        ));
    }
    let client = client.ok_or("fetching a capture by id needs the workspace's login")?;
    client
        .get_json(&format!("/api/v1/manage/captures/{source}"))
        .await
        .map_err(|e| format!("capture {source}: {e}"))
}

/// What one `--from-trace` run did.
#[derive(Debug)]
pub(crate) struct Written {
    pub domain: String,
    pub file: PathBuf,
    pub generated: Generated,
    pub reproduction: Reproduction,
}

/// Write the capture's test into the domain that defines its op, and run it
/// once. `Err` is a test not written, and why.
pub(crate) fn write(
    root: &Path,
    domain: Option<&str>,
    capture: &OpCapture,
) -> Result<Written, String> {
    if !root.join("workspace.json").exists() {
        return Err(format!(
            "no workspace.json at {} — `forge test --from-trace` writes into a workspace; \
             pass --manifest-dir or run from the workspace root",
            root.display()
        ));
    }
    let (tables, _) = dialect::workspace_tables(root)?;
    let contracts = dialect::contract_surfaces(root)?;
    for d in dialect::domains(root) {
        let name = dialect::domain_name(&d);
        if domain.is_some_and(|wanted| wanted != name) {
            continue;
        }
        let sources = dialect::sources_of(&d);
        if sources.is_empty() {
            continue;
        }
        let under = DomainUnderTest {
            name: name.clone(),
            sources,
            tests: test_files(&d),
            tables: tables.as_ref(),
            contracts: &contracts,
            inputs: dialect::input_schemas(&d).map_err(|e| e.to_string())?,
        };
        let built = bundle(&under).map_err(|f| format!("domain `{name}`: {}", f.message))?;
        let Some(lane) = from_trace::lane_of(&built, &capture.op) else {
            continue;
        };
        let generated = from_trace::generate(capture, lane)?;
        let file = from_trace::write(&d, &generated)?;
        let reproduction = from_trace::reproduce(&built, &generated, tables.as_ref());
        return Ok(Written {
            domain: name,
            file,
            generated,
            reproduction,
        });
    }
    Err(format!(
        "no domain{} under {}/domains/ defines op `{}`; the capture is of an op this \
         workspace's tree no longer has",
        domain.map(|d| format!(" `{d}`")).unwrap_or_default(),
        root.display(),
        capture.op
    ))
}

/// The human report.
pub(crate) fn human(root: &Path, capture: &OpCapture, w: &Written) -> String {
    let file = w.file.strip_prefix(root).unwrap_or(&w.file).display();
    let mut out = format!(
        "wrote {file} ({}): {}\n",
        from_trace::lane_name(w.generated.lane),
        w.generated.case.name
    );
    match &w.reproduction.why {
        None => out.push_str(&format!(
            "  it fails with {}, as op `{}` did in production; fix the op, then `forge test`\n",
            w.generated.expected, capture.op
        )),
        Some(why) => out.push_str(&format!("  it does NOT reproduce the failure: {why}\n")),
    }
    if !w.generated.redactions.is_empty() {
        out.push_str("  redacted at capture (named in the file's header):\n");
        for r in &w.generated.redactions {
            out.push_str(&format!("    {} ({})\n", r.at, r.path));
        }
    }
    for note in &w.generated.notes {
        out.push_str(&format!("  note: {note}\n"));
    }
    out
}

/// The `forge-from-trace/1` envelope, the file relative to the workspace.
pub(crate) fn envelope(root: &Path, capture: &OpCapture, w: &Written) -> serde_json::Value {
    let file = w
        .file
        .strip_prefix(root)
        .unwrap_or(&w.file)
        .display()
        .to_string();
    from_trace::envelope(capture, &w.domain, &file, &w.generated, &w.reproduction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_platform_wire::{CaptureEntry, CaptureError, CaptureOutcome};
    use serde_json::json;

    const OP: &str = r#"from forge import OpContext, OpError, Value, op, storage
from forge_schema.inputs.billing import InvoiceDueRequest, InvoiceDueResponse


@op
def invoice_due(ctx: OpContext, req: InvoiceDueRequest) -> InvoiceDueResponse:
    resp: Value = storage.query(
        {
            "from": "invoices",
            "filter": {"column": "id", "op": "eq", "value": req.invoice},
            "limit": 1,
        }
    )
    due: int = 0
    for row in storage.rows(resp):
        if row.get("paid", 0) > row.get("amount", 0):
            raise OpError.bad_request("an invoice is never overpaid")
        due = row.get("amount", 0) - row.get("paid", 0)
    return InvoiceDueResponse(due=due)
"#;

    const FIXED: &str = r#"from forge import OpContext, Value, op, storage
from forge_schema.inputs.billing import InvoiceDueRequest, InvoiceDueResponse


@op
def invoice_due(ctx: OpContext, req: InvoiceDueRequest) -> InvoiceDueResponse:
    resp: Value = storage.query(
        {
            "from": "invoices",
            "filter": {"column": "id", "op": "eq", "value": req.invoice},
            "limit": 1,
        }
    )
    due: int = 0
    for row in storage.rows(resp):
        amount: int = row.get("amount", 0)
        paid: int = row.get("paid", 0)
        due = max(amount - paid, 0)
    return InvoiceDueResponse(due=due)
"#;

    const SERVICE: &str = r#"{"domain": "billing", "operations": [{"name": "invoice_due",
"input_schema": {"type": "object", "properties": {"invoice": {"type": "string"}}, "required": ["invoice"]},
"output_schema": {"type": "object", "properties": {"due": {"type": "integer"}}, "required": ["due"]}}]}"#;

    const SCHEMA: &str = r#"{"name": "invoices", "archetype": "Base", "label": "Invoice",
"plural_label": "Invoices", "columns": [
{"name": "customer_email", "type": "string", "required": true, "label": "Customer email", "pii": true},
{"name": "amount", "type": "integer", "required": true, "label": "Amount"},
{"name": "paid", "type": "integer", "required": true, "label": "Paid"}]}"#;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in [
            ("workspace.json", r#"{"name": "acme"}"#),
            ("domains/billing/service.json", SERVICE),
            ("domains/billing/services/invoice_due.py", OP),
            ("domains/billing/schemas/invoices.table.json", SCHEMA),
        ] {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    /// The capture the runtime stores for the overpaid invoice, its pii
    /// column redacted.
    fn capture() -> OpCapture {
        OpCapture {
            op: "invoice_due".into(),
            input: r#"{"invoice": "inv-9"}"#.into(),
            outcome: CaptureOutcome::Error(CaptureError {
                kind: "bad_request".into(),
                message: "an invoice is never overpaid".into(),
                declared: None,
            }),
            entries: vec![CaptureEntry {
                iface: "storage.query".into(),
                req: json!({"from": "invoices", "filter": {"column": "id", "op": "eq", "value": "inv-9"}, "limit": 1}),
                resp: json!({"rows": [{"id": "inv-9", "customer_email": "[redacted]", "amount": 500, "paid": 700}]}),
            }],
            redactions: vec!["entries[0].resp.rows[0].customer_email".into()],
            captured_at: 1_791_520_000_000_000,
            identity: Default::default(),
            id: Some("01ja2m7q9zk4".into()),
        }
    }

    #[test]
    fn a_capture_file_becomes_a_test_that_fails_until_the_fix() {
        let ws = workspace();
        let root = ws.path();
        let file = root.join("capture.json");
        std::fs::write(&file, serde_json::to_string(&capture()).unwrap()).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let loaded = rt
            .block_on(load(file.to_str().unwrap(), None))
            .expect("a capture file loads with no login");
        let written = write(root, None, &loaded).unwrap();
        assert!(
            written.reproduction.reproduced,
            "{:?}",
            written.reproduction
        );
        let doc = envelope(root, &loaded, &written);
        assert_eq!(doc["schema"], "forge-from-trace/1");
        assert_eq!(
            doc["file"],
            "domains/billing/tests/test_invoice_due_capture_01ja2m7q.py"
        );
        assert_eq!(doc["lane"], "python");
        assert_eq!(doc["reproduced"], true);
        assert_eq!(
            doc["failure"]["error"],
            json!({"code": "bad_request", "message": "an invoice is never overpaid"})
        );
        assert_eq!(
            doc["redactions"][0]["at"],
            "`invoices` seed row 1, `customer_email`"
        );
        let text = std::fs::read_to_string(root.join(doc["file"].as_str().unwrap())).unwrap();
        assert!(text.contains(r#""customer_email": "[redacted]""#), "{text}");

        let before = crate::cmd::test::collect(root, None, None, None).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].failure.as_ref().unwrap().kind, "op_error");

        std::fs::write(root.join("domains/billing/services/invoice_due.py"), FIXED).unwrap();
        let after = crate::cmd::test::collect(root, None, None, None).unwrap();
        assert!(crate::cmd::test::all_passed(&after), "{after:#?}");

        let again = write(root, None, &loaded).unwrap_err();
        assert!(again.contains("already exists"), "{again}");
    }

    #[test]
    fn a_capture_of_an_op_the_tree_lacks_or_a_bad_id_writes_nothing() {
        let ws = workspace();
        let mut gone = capture();
        gone.op = "renamed_away".into();
        let refused = write(ws.path(), None, &gone).unwrap_err();
        assert!(refused.contains("defines op `renamed_away`"), "{refused}");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let bad = rt.block_on(load("../etc/passwd", None)).unwrap_err();
        assert!(
            bad.contains("neither a capture file nor a capture id"),
            "{bad}"
        );
        let no_login = rt.block_on(load("01ja2m7q", None)).unwrap_err();
        assert!(no_login.contains("login"), "{no_login}");
    }
}
