//! ST-6: a contract finding as `forge check` shows it — a named diff between
//! an op's code and what its `service.json` declares, one row per field
//! (`forge-lang/ST-6-DESIGN.md` decision 2.3).
//!
//! The diff is the checker's structured [`ContractDiff`], built where the
//! rule decided; nothing here reads a message back. The human rendering and
//! the `contract_mismatches` JSON are two spellings of the same value.

use forge_lang_rustgen::{ContractDiff, DiffRow, Facet};
use serde_json::{Value, json};

/// Where a row's left column ends, so the verdicts line up.
const ROW_WIDTH: usize = 24;

/// The facet as the header names it.
fn facet_word(facet: Facet) -> &'static str {
    match facet {
        Facet::Input => "input",
        Facet::Output => "output",
        Facet::Errors => "errors",
    }
}

/// `billing::charge` when the op's `service.json` is in scope, else the op.
fn qualified(diff: &ContractDiff) -> String {
    let domain = diff.service_json.as_ref().and_then(|at| {
        at.path
            .strip_prefix("domains/")
            .and_then(|rest| rest.strip_suffix("/service.json"))
    });
    match domain {
        Some(domain) => format!("{domain}::{}", diff.op),
        None => diff.op.clone(),
    }
}

/// One diff, newline-terminated:
///
/// ```text
/// billing::charge: code and service.json disagree on output
///   domains/billing/service.json /operations/0/output_schema   vs   domains/billing/services/charge.py:14
///   - id: string            missing in code
///   + note                  not declared
///   ~ amount: integer       code writes float
/// ```
pub(crate) fn render(diff: &ContractDiff) -> String {
    let mut out = format!(
        "{}: code and service.json disagree on {}\n",
        qualified(diff),
        facet_word(diff.facet)
    );
    let declared = diff.service_json.as_ref().map_or_else(
        || "no service.json in scope".to_string(),
        |at| format!("{} {}", at.path, at.pointer),
    );
    match &diff.code {
        Some(code) => out.push_str(&format!("  {declared}   vs   {code}\n")),
        None => out.push_str(&format!("  {declared}\n")),
    }
    for row in &diff.rows {
        let line = match row {
            DiffRow::Missing { name, ty } => {
                format!("- {:<ROW_WIDTH$}missing in code", format!("{name}: {ty}"))
            }
            DiffRow::Extra { name } => format!("+ {name:<ROW_WIDTH$}not declared"),
            DiffRow::Mistyped {
                name,
                declared,
                found,
            } => format!(
                "~ {:<ROW_WIDTH$}code writes {found}",
                format!("{name}: {declared}")
            ),
            DiffRow::UndeclaredCode { code } => format!("! raises undeclared code {code}"),
            DiffRow::HandWritten {
                type_name,
                generated,
            } => {
                // The generated request is taken, the generated response is
                // returned: the verb follows the facet the type stands in for.
                let verb = match diff.facet {
                    Facet::Input => "take",
                    Facet::Output | Facet::Errors => "return",
                };
                format!("! {type_name} is hand-written; {verb} the generated {generated}")
            }
        };
        out.push_str(&format!("  {}\n", line.trim_end()));
    }
    out
}

/// One diff as an element of the envelope's `contract_mismatches`.
pub(crate) fn to_json(diff: &ContractDiff) -> Value {
    let rows: Vec<Value> = diff
        .rows
        .iter()
        .map(|row| match row {
            DiffRow::Missing { name, ty } => json!({"kind": "missing", "name": name, "type": ty}),
            DiffRow::Extra { name } => json!({"kind": "extra", "name": name}),
            DiffRow::Mistyped {
                name,
                declared,
                found,
            } => json!({"kind": "mistyped", "name": name, "declared": declared, "found": found}),
            DiffRow::UndeclaredCode { code } => json!({"kind": "undeclared_code", "code": code}),
            DiffRow::HandWritten {
                type_name,
                generated,
            } => json!({"kind": "hand_written", "type_name": type_name, "generated": generated}),
        })
        .collect();
    json!({
        "op": diff.op,
        "facet": facet_word(diff.facet),
        "service_json": diff.service_json.as_ref().map(|at| json!({
            "path": at.path,
            "pointer": at.pointer,
        })),
        "code": diff.code,
        "rows": rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::check::{Verdicts, check};
    use forge_lang_rustgen::ServiceJsonAt;

    /// A hand-written boundary type renders as the generated type to use in
    /// its place. Built directly: the Python lane's `contract-at-boundary`
    /// rule that produces this row is not on the forge-lang this binary links.
    #[test]
    fn a_hand_written_boundary_type_renders_as_the_generated_one_to_return() {
        let diff = ContractDiff {
            op: "charge".into(),
            facet: Facet::Output,
            service_json: Some(ServiceJsonAt {
                path: "domains/billing/service.json".into(),
                pointer: "/operations/0/output_schema".into(),
            }),
            code: Some("domains/billing/services/charge.py:14".into()),
            rows: vec![DiffRow::HandWritten {
                type_name: "ChargeResult".into(),
                generated: "ChargeResponse".into(),
            }],
        };
        assert_eq!(
            render(&diff),
            "billing::charge: code and service.json disagree on output\n  \
             domains/billing/service.json /operations/0/output_schema   vs   \
             domains/billing/services/charge.py:14\n  \
             ! ChargeResult is hand-written; return the generated ChargeResponse\n"
        );
        assert_eq!(
            to_json(&diff),
            json!({
                "op": "charge",
                "facet": "output",
                "service_json": {
                    "path": "domains/billing/service.json",
                    "pointer": "/operations/0/output_schema",
                },
                "code": "domains/billing/services/charge.py:14",
                "rows": [{
                    "kind": "hand_written",
                    "type_name": "ChargeResult",
                    "generated": "ChargeResponse",
                }],
            })
        );
    }

    /// Every row kind lines up under the design's layout.
    #[test]
    fn each_row_kind_renders_in_the_designs_layout() {
        let diff = ContractDiff {
            op: "charge".into(),
            facet: Facet::Output,
            service_json: None,
            code: None,
            rows: vec![
                DiffRow::Missing {
                    name: "id".into(),
                    ty: "string".into(),
                },
                DiffRow::Extra {
                    name: "note".into(),
                },
                DiffRow::Mistyped {
                    name: "amount".into(),
                    declared: "integer".into(),
                    found: "float".into(),
                },
            ],
        };
        assert_eq!(
            render(&diff),
            "charge: code and service.json disagree on output\n  \
             no service.json in scope\n  \
             - id: string              missing in code\n  \
             + note                    not declared\n  \
             ~ amount: integer         code writes float\n"
        );
    }

    // ── through `forge check`, one workspace per facet ──────────────────

    /// `forge-lang`'s `fixtures/contract-once/service.json`: `ship_quote`
    /// declares its input and output, `ship_cancel` its one error code.
    const SHIPPING: &str = r#"{
  "domain": "shipping",
  "operations": [
    {
      "name": "ship_quote",
      "input_schema": {
        "type": "object",
        "properties": {"weight": {"type": "integer"}, "zone": {"type": "string"}},
        "required": ["weight", "zone"]
      },
      "output_schema": {
        "type": "object",
        "properties": {"cents": {"type": "integer"}, "zone": {"type": "string"}},
        "required": ["cents", "zone"]
      }
    },
    {
      "name": "ship_track",
      "input_schema": {
        "type": "object",
        "properties": {"tracking": {"type": "string"}},
        "required": ["tracking"]
      }
    },
    {
      "name": "ship_cancel",
      "input_schema": {
        "type": "object",
        "properties": {"tracking": {"type": "string"}},
        "required": ["tracking"]
      },
      "errors": [
        {
          "code": "already_shipped",
          "status": 409,
          "message": "the shipment has already left",
          "details_schema": {
            "type": "object",
            "properties": {"tracking": {"type": "string"}},
            "required": ["tracking"]
          }
        }
      ]
    }
  ]
}
"#;

    /// Line 1-3 of every op source: the imports, then two blank lines.
    const HEAD: &str = "from dataclasses import dataclass\n\
                        from forge import OpContext, op\n\
                        from forge_schema.inputs.shipping import AlreadyShipped, \
                        ShipQuoteRequest, ShipQuoteResponse, ShipTrackRequest\n\n\n";

    const QUOTE: &str =
        "@op\ndef ship_quote(ctx: OpContext, req: ShipQuoteRequest) -> ShipQuoteResponse:\n";

    /// `forge check` over a workspace holding `service` and one op source at
    /// `domains/shipping/services/<file>`: the verdicts, or `None` on a
    /// machine with no CPython.
    fn checked(service: &str, file: &str, body: &str) -> Option<(tempfile::TempDir, Verdicts)> {
        let ws = tempfile::tempdir().unwrap();
        let services = ws.path().join("domains/shipping/services");
        std::fs::create_dir_all(&services).unwrap();
        std::fs::write(ws.path().join("workspace.json"), "{}").unwrap();
        std::fs::write(ws.path().join("domains/shipping/service.json"), service).unwrap();
        std::fs::write(services.join(file), format!("{HEAD}{body}")).unwrap();
        match check(ws.path()) {
            Ok(found) => Some((ws, found)),
            Err(e) if e.contains("python3") || e.contains("CPython") => {
                eprintln!("skipping: {e}");
                None
            }
            Err(e) => panic!("{e}"),
        }
    }

    /// The one diff about `facet`, with the workspace's temp root cut from
    /// its code location so the expectation is stable.
    #[track_caller]
    fn the_diff(ws: &tempfile::TempDir, found: &Verdicts, facet: Facet) -> ContractDiff {
        assert!(!found.ok(), "a contract finding must fail the run (exit 1)");
        let mut d = found
            .contract_mismatches
            .iter()
            .find(|d| d.facet == facet)
            .unwrap_or_else(|| panic!("no {facet:?} diff in {:#?}", found.contract_mismatches))
            .clone();
        let prefix = format!("{}/", ws.path().display());
        d.code = d.code.map(|c| c.replace(&prefix, ""));
        d
    }

    #[test]
    fn check_shows_an_output_the_code_leaves_a_declared_field_out_of() {
        let body = format!("{QUOTE}    return ShipQuoteResponse(cents=req.weight)\n");
        let Some((ws, found)) = checked(SHIPPING, "ship_quote.py", &body) else {
            return;
        };
        let d = the_diff(&ws, &found, Facet::Output);
        assert_eq!(
            render(&d),
            "shipping::ship_quote: code and service.json disagree on output\n  \
             domains/shipping/service.json /operations/0/output_schema   vs   \
             domains/shipping/services/ship_quote.py:8\n  \
             - zone: str               missing in code\n"
        );
        assert_eq!(
            to_json(&d),
            json!({
                "op": "ship_quote",
                "facet": "output",
                "service_json": {
                    "path": "domains/shipping/service.json",
                    "pointer": "/operations/0/output_schema",
                },
                "code": "domains/shipping/services/ship_quote.py:8",
                "rows": [{"kind": "missing", "name": "zone", "type": "str"}],
            })
        );
    }

    #[test]
    fn check_shows_an_input_field_the_code_reads_and_service_json_does_not_declare() {
        let body =
            format!("{QUOTE}    return ShipQuoteResponse(cents=req.weight, zone=req.region)\n");
        let Some((ws, found)) = checked(SHIPPING, "ship_quote.py", &body) else {
            return;
        };
        let d = the_diff(&ws, &found, Facet::Input);
        assert_eq!(
            render(&d),
            "shipping::ship_quote: code and service.json disagree on input\n  \
             domains/shipping/service.json /operations/0/input_schema   vs   \
             domains/shipping/services/ship_quote.py:8\n  \
             + region                  not declared\n"
        );
        assert_eq!(
            to_json(&d),
            json!({
                "op": "ship_quote",
                "facet": "input",
                "service_json": {
                    "path": "domains/shipping/service.json",
                    "pointer": "/operations/0/input_schema",
                },
                "code": "domains/shipping/services/ship_quote.py:8",
                "rows": [{"kind": "extra", "name": "region"}],
            })
        );
    }

    #[test]
    fn check_shows_a_raise_of_a_code_the_op_does_not_declare() {
        let body = "@dataclass\nclass Tracked:\n    tracking: str\n\n\n\
                    @op\ndef ship_track(ctx: OpContext, req: ShipTrackRequest) -> Tracked:\n    \
                    raise AlreadyShipped(tracking=req.tracking)\n";
        let Some((ws, found)) = checked(SHIPPING, "ship_track.py", body) else {
            return;
        };
        let d = the_diff(&ws, &found, Facet::Errors);
        assert_eq!(
            render(&d),
            "shipping::ship_track: code and service.json disagree on errors\n  \
             domains/shipping/service.json /operations/1/errors   vs   \
             domains/shipping/services/ship_track.py:13\n  \
             ! raises undeclared code already_shipped\n"
        );
        assert_eq!(
            to_json(&d),
            json!({
                "op": "ship_track",
                "facet": "errors",
                "service_json": {
                    "path": "domains/shipping/service.json",
                    "pointer": "/operations/1/errors",
                },
                "code": "domains/shipping/services/ship_track.py:13",
                "rows": [{"kind": "undeclared_code", "code": "already_shipped"}],
            })
        );
    }

    /// Code and `service.json` agree: no contract mismatch, and the run
    /// passes (exit 0).
    #[test]
    fn check_passes_a_workspace_whose_code_and_service_json_agree() {
        let service = r#"{
  "domain": "shipping",
  "operations": [
    {
      "name": "ship_quote",
      "input_schema": {
        "type": "object",
        "properties": {"weight": {"type": "integer"}, "zone": {"type": "string"}},
        "required": ["weight", "zone"]
      },
      "output_schema": {
        "type": "object",
        "properties": {"cents": {"type": "integer"}, "zone": {"type": "string"}},
        "required": ["cents", "zone"]
      }
    }
  ]
}
"#;
        let body =
            format!("{QUOTE}    return ShipQuoteResponse(cents=req.weight * 3, zone=req.zone)\n");
        let Some((_ws, found)) = checked(service, "ship_quote.py", &body) else {
            return;
        };
        assert!(
            found.contract_mismatches.is_empty(),
            "{:#?}",
            found.contract_mismatches
        );
        assert!(found.ok(), "{:#?}", found.registers);
    }
}
