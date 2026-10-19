"""The `{{domain}}::list_notes` op."""

from forge import OpContext, op
from forge_schema.inputs.{{domain}} import ListNotesRequest, ListNotesResponse, NoteRow
from forge_schema.tables import notes


@op
def list_notes(ctx: OpContext, req: ListNotesRequest) -> ListNotesResponse:
    """The notes the caller may read."""
    rows = notes.query().limit(100).fetch()
    return ListNotesResponse(items=[NoteRow(id=r.id, body=r.body) for r in rows])
