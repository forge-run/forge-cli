//! The `{{domain}}::add_note` op.

use forge::schema::inputs::{{domain}}::{AddNoteRequest, AddNoteResponse};
use forge::schema::notes;
use forge::{op, OpContext};

/// Writes one note as the caller.
#[op]
#[must_use]
pub fn add_note(_ctx: &OpContext, req: &AddNoteRequest) -> AddNoteResponse {
    let _written = notes::insert(notes::new_row().set(notes::BODY, &req.body)).execute();
    AddNoteResponse {
        body: req.body.clone(),
    }
}
