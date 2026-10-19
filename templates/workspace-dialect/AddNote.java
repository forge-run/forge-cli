package forge.app;

import forge.Op;
import forge.OpContext;
import forge.Value;
import forge.schema.Notes;
import forge.schema.inputs.{{domain}}.AddNoteRequest;
import forge.schema.inputs.{{domain}}.AddNoteResponse;

/** The {@code {{domain}}::add_note} op. */
public final class AddNote {

  /** Writes one note as the caller. */
  @Op
  public static AddNoteResponse addNote(OpContext ctx, AddNoteRequest req) {
    Value written = Notes.insert(Notes.newRow().set(Notes.BODY, req.body())).execute();
    return new AddNoteResponse(req.body());
  }
}
