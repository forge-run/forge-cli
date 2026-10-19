// The page at `/notes`, a client page in the app's layout. Its load is live:
// when a row of `notes` the viewer may read changes, here or in another
// browser, the page fetches `list_notes` again and repaints.
import { Node, Loaded, load } from "forge";
import { LoadFailed } from "forge/ui";
import { ListNotesResponse } from "forge/schema";
import AppLayout from "../layouts/app";
import NoteComposer from "../components/note_composer";

export const mode = "client";
export const layout = AppLayout;
export const loader = { notes: load("list_notes", {}, { live: true }) };

export interface NotesProps {
  notes: Loaded<ListNotesResponse>;
}

export default function Notes({ notes }: NotesProps): Node {
  switch (notes.kind) {
    case "failed":
      return <LoadFailed reason={notes.reason} traceId={notes.traceId} />;
    case "ok":
      return (
        <section>
          <h1>Notes</h1>
          {notes.value.items.length === 0 ? <p>No notes yet.</p> : null}
          <ul id="notes">
            {notes.value.items.map((n) => (
              <li key={n.id}>{n.body}</li>
            ))}
          </ul>
          <NoteComposer />
        </section>
      );
  }
}
