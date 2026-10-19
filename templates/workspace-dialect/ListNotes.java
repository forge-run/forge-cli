package forge.app;

import forge.Op;
import forge.OpContext;
import forge.schema.Notes;
import forge.schema.inputs.{{domain}}.ListNotesRequest;
import forge.schema.inputs.{{domain}}.ListNotesResponse;
import forge.schema.inputs.{{domain}}.NoteRow;
import java.util.List;

/** The {@code {{domain}}::list_notes} op. */
public final class ListNotes {

  /** The notes the caller may read. */
  @Op
  public static ListNotesResponse listNotes(OpContext ctx, ListNotesRequest req) {
    List<Notes.Row> rows = Notes.query().limit(100).fetch();
    List<NoteRow> items = rows.stream().map(r -> new NoteRow(r.id(), r.body())).toList();
    return new ListNotesResponse(items);
  }
}
