"""The `{{domain}}::add_note` op."""

from forge import OpContext, Value, op
from forge_schema.inputs.{{domain}} import AddNoteRequest, AddNoteResponse
from forge_schema.tables import notes


@op
def add_note(ctx: OpContext, req: AddNoteRequest) -> AddNoteResponse:
    """Write one note as the caller."""
    written: Value = notes.insert(notes.new_row().set(notes.BODY, req.body)).execute()
    return AddNoteResponse(body=req.body)
