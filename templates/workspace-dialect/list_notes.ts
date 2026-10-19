/** The `{{domain}}::list_notes` op: the notes the caller may read. */
import { op } from "forge";
import { ListNotesResponse, NoteRow } from "forge/schema";
import { Notes } from "forge/schema/tables";

export const listNotes = op((): ListNotesResponse => {
  const rows = Notes.query().limit(100).fetch();
  const items: NoteRow[] = [];
  for (const r of rows) {
    items.push({ id: r.id, body: r.body });
  }
  return { items };
});
