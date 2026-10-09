//! What `forge push` reports about the push's behaviour diff (OR-2): the
//! `forge-push/1` envelope `--json` prints, and the human lines without it.
//!
//! The runtime replays its recent captures on the live and the candidate tree
//! before the swap and serves the result on `reconcile/status` as
//! `behaviour_diff` ([`forge_platform_wire::BehaviourDiff`]). A diff is only
//! reported when its `desired_hash` is the status's own: a diff left over from
//! an earlier push is about a different tree.
//!
//! A push that changes the schema carries OR-4's findings in the same diff:
//! `schema_changes` and the captured requests they reach, `schema_findings`.

use forge_platform_wire::{BehaviourDiff, CaptureOutcome, ReplayAnswer, SchemaFindingKind};

use super::push::Reconcile;

/// The envelope's schema tag. Fields are additive: a new one never changes
/// the meaning of one already there.
pub(super) const PUSH_SCHEMA: &str = "forge-push/1";

/// The status's diff, when it is about the status's desired hash.
pub(super) fn matching_diff(st: &Reconcile) -> Option<&BehaviourDiff> {
    st.behaviour_diff.as_ref().filter(|d| {
        !st.desired_hash.is_empty() && d.desired_hash.as_deref() == Some(st.desired_hash.as_str())
    })
}

/// The `forge-push/1` envelope for the last status the push saw.
pub(super) fn envelope(st: &Reconcile) -> serde_json::Value {
    let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
    serde_json::json!({
        "schema": PUSH_SCHEMA,
        "git_sha": opt(&st.git_sha),
        "desired_hash": opt(&st.desired_hash),
        "live_hash": opt(&st.live_hash),
        "in_sync": st.in_sync,
        "last_error": st.last_error,
        "behaviour_diff": matching_diff(st),
    })
}

/// The human report: one line per op, then each example the runtime kept.
pub(super) fn render(diff: Option<&BehaviourDiff>) -> Vec<String> {
    let Some(diff) = diff else {
        return vec!["no behaviour diff was reported for this push".to_string()];
    };
    let mut lines = vec![format!(
        "behaviour diff over {} captured request(s):",
        diff.captures
    )];
    for op in &diff.ops {
        let mut notes = Vec::new();
        if op.missing_in_candidate {
            notes.push("op missing in the candidate".to_string());
        }
        if op.not_replayed > 0 {
            notes.push(format!("{} not replayed", op.not_replayed));
        }
        let mut line = format!(
            "{}: {} of {} captured requests changed",
            op.op, op.changed, op.replayed
        );
        if !notes.is_empty() {
            line.push_str(&format!(" ({})", notes.join("; ")));
        }
        lines.push(line);
        for ex in &op.examples {
            match &ex.capture_id {
                Some(id) => lines.push(format!("  capture {id}")),
                None => lines.push("  capture".to_string()),
            }
            lines.push(format!("    input:  {}", ex.input));
            lines.push(format!("    before: {}", answer(&ex.before)));
            lines.push(format!("    after:  {}", answer(&ex.after)));
            if let Some(at) = &ex.diverged_at
                && ex.after.diverged_at.as_ref() != Some(at)
            {
                lines.push(format!("    diverged at {at}"));
            }
        }
    }
    lines.extend(render_schema(diff));
    if let Some(err) = &diff.replay_error {
        lines.push(format!("replay error: {err}"));
    }
    lines
}

/// OR-4's lines: nothing when the push changes no column a capture can
/// feel; otherwise the count, then each finding with its captures.
fn render_schema(diff: &BehaviourDiff) -> Vec<String> {
    if diff.schema_changes.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "schema change replay over {} change(s): {} finding(s)",
        diff.schema_changes.len(),
        diff.schema_findings.len()
    )];
    for f in &diff.schema_findings {
        let what = match f.kind {
            SchemaFindingKind::DroppedColumnRead => "reads dropped column",
            SchemaFindingKind::NarrowedTypeRead => "reads narrowed column",
            SchemaFindingKind::NotNullWrite => "writes without new NOT NULL column",
        };
        let fails = f.captures.iter().filter(|c| c.fails).count();
        lines.push(format!(
            "{}: {what} {}.{}: {} captured request(s), {fails} would fail",
            f.op,
            f.table,
            f.column,
            f.captures.len()
        ));
        for c in &f.captures {
            match &c.capture_id {
                Some(id) => lines.push(format!("  capture {id}")),
                None => lines.push("  capture".to_string()),
            }
            lines.push(format!("    input:  {}", c.input));
            lines.push(format!("    {}", c.detail));
        }
    }
    lines
}

fn answer(a: &ReplayAnswer) -> String {
    let mut s = match &a.outcome {
        CaptureOutcome::Output(out) => format!("output {out}"),
        CaptureOutcome::Error(e) if e.message.is_empty() => format!("error {}", e.kind),
        CaptureOutcome::Error(e) => format!("error {}: {}", e.kind, e.message),
    };
    if let Some(at) = &a.diverged_at {
        s.push_str(&format!(" (diverged at {at})"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_platform_wire::{CaptureError, DiffExample, OpDiff};

    fn diff(desired: &str) -> BehaviourDiff {
        BehaviourDiff {
            desired_hash: Some(desired.into()),
            live_hash: Some("a1e2".into()),
            captures: 5,
            ops: vec![
                OpDiff {
                    op: "quote".into(),
                    replayed: 4,
                    changed: 1,
                    examples: vec![DiffExample {
                        capture_id: Some("cap-2".into()),
                        input: r#"{"cents": 150}"#.into(),
                        before: ReplayAnswer {
                            outcome: CaptureOutcome::Output(r#"{"units":1}"#.into()),
                            effects: vec![],
                            diverged_at: None,
                        },
                        after: ReplayAnswer {
                            outcome: CaptureOutcome::Error(CaptureError {
                                kind: "bad_request".into(),
                                message: "no price".into(),
                                declared: None,
                            }),
                            effects: vec![],
                            diverged_at: Some("notifications.send_email".into()),
                        },
                        diverged_at: Some("notifications.send_email".into()),
                    }],
                    missing_in_candidate: false,
                    not_replayed: 0,
                },
                OpDiff {
                    op: "retired".into(),
                    replayed: 0,
                    changed: 0,
                    examples: vec![],
                    missing_in_candidate: true,
                    not_replayed: 1,
                },
            ],
            replay_error: Some("replay budget of 5s ran out after 5 captures".into()),
            ..BehaviourDiff::default()
        }
    }

    fn status(diff: Option<BehaviourDiff>) -> Reconcile {
        let mut st = Reconcile::default();
        st.git_sha = "abc123".into();
        st.desired_hash = "b3f1".into();
        st.live_hash = "b3f1".into();
        st.in_sync = true;
        st.behaviour_diff = diff;
        st
    }

    #[test]
    fn the_report_names_changed_and_missing_ops_and_the_replay_error() {
        let lines = render(Some(&diff("b3f1")));
        assert_eq!(
            lines,
            [
                "behaviour diff over 5 captured request(s):",
                "quote: 1 of 4 captured requests changed",
                "  capture cap-2",
                r#"    input:  {"cents": 150}"#,
                r#"    before: output {"units":1}"#,
                "    after:  error bad_request: no price (diverged at notifications.send_email)",
                "retired: 0 of 0 captured requests changed (op missing in the candidate; 1 not replayed)",
                "replay error: replay budget of 5s ran out after 5 captures",
            ]
        );
    }

    #[test]
    fn the_report_names_each_schema_finding_and_its_captures() {
        use forge_platform_wire::{
            SchemaChange, SchemaChangeKind, SchemaFinding, SchemaFindingCapture,
        };
        let mut d = diff("b3f1");
        d.ops.clear();
        d.replay_error = None;
        d.schema_changes = vec![SchemaChange {
            kind: SchemaChangeKind::DroppedColumn,
            table: "notes".into(),
            column: "label".into(),
            from: None,
            to: None,
        }];
        d.schema_findings = vec![SchemaFinding {
            kind: SchemaFindingKind::DroppedColumnRead,
            op: "note_label".into(),
            table: "notes".into(),
            column: "label".into(),
            captures: vec![SchemaFindingCapture {
                capture_id: Some("cap-7".into()),
                input: r#"{"id": "n1"}"#.into(),
                fails: true,
                detail: "the recorded rows of `notes` no longer carry `label`, and the op now \
                         ends in an error"
                    .into(),
                after: None,
            }],
        }];
        assert_eq!(
            render(Some(&d)),
            [
                "behaviour diff over 5 captured request(s):",
                "schema change replay over 1 change(s): 1 finding(s)",
                "note_label: reads dropped column notes.label: 1 captured request(s), 1 would fail",
                "  capture cap-7",
                r#"    input:  {"id": "n1"}"#,
                "    the recorded rows of `notes` no longer carry `label`, and the op now ends in \
                 an error",
            ]
        );
        d.schema_findings.clear();
        assert_eq!(
            render(Some(&d))[1],
            "schema change replay over 1 change(s): 0 finding(s)"
        );
    }

    #[test]
    fn no_diff_is_one_line() {
        assert_eq!(
            render(None),
            ["no behaviour diff was reported for this push"]
        );
    }

    #[test]
    fn the_envelope_carries_the_schema_tag_and_the_matching_diff() {
        let doc = envelope(&status(Some(diff("b3f1"))));
        let keys: Vec<&str> = doc
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "behaviour_diff",
                "desired_hash",
                "git_sha",
                "in_sync",
                "last_error",
                "live_hash",
                "schema"
            ]
        );
        assert_eq!(doc["schema"], "forge-push/1");
        assert_eq!(doc["git_sha"], "abc123");
        assert_eq!(doc["in_sync"], true);
        assert!(doc["last_error"].is_null());
        assert_eq!(
            doc["behaviour_diff"],
            serde_json::to_value(diff("b3f1")).unwrap()
        );
    }

    #[test]
    fn the_envelope_reports_null_when_the_diff_is_absent_or_for_another_push() {
        assert!(envelope(&status(None))["behaviour_diff"].is_null());
        let stale = envelope(&status(Some(diff("0ld0"))));
        assert!(stale["behaviour_diff"].is_null());
        assert_eq!(stale["desired_hash"], "b3f1");
    }

    #[test]
    fn a_failed_converge_still_has_an_envelope_with_its_error() {
        let mut st = status(None);
        st.in_sync = false;
        st.last_error = Some("destructive schema delta".into());
        st.git_sha.clear();
        let doc = envelope(&st);
        assert_eq!(doc["last_error"], "destructive schema delta");
        assert_eq!(doc["in_sync"], false);
        assert!(doc["git_sha"].is_null());
    }
}
