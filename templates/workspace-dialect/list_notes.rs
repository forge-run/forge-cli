//! The `{{domain}}::list_notes` op.

use forge::schema::inputs::{{domain}}::{ListNotesRequest, ListNotesResponse, NoteRow};
use forge::schema::notes;
use forge::{op, OpContext};

/// The notes the caller may read.
#[op]
#[must_use]
pub fn list_notes(_ctx: &OpContext, _req: &ListNotesRequest) -> ListNotesResponse {
    let rows = notes::query().limit(100).fetch();
    ListNotesResponse {
        items: rows
            .iter()
            .map(|r| NoteRow {
                id: r.id.clone(),
                body: r.body.clone(),
            })
            .collect(),
    }
}
