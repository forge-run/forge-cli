// SPA-6: a page whose notes re-run when a note changes, as the viewer
// sees them.
import { Node, Loaded, load } from "forge";
import { ListNotesResponse } from "forge/schema";

export const loader = {
  notes: load("list_notes", {}, { live: true }),
};

export interface NotesProps {
  notes: Loaded<ListNotesResponse>;
}

export default function Notes({ notes }: NotesProps): Node {
  switch (notes.kind) {
    case "failed":
      return <p>{notes.reason}</p>;
    case "ok":
      return (
        <ul>
          {notes.value.items.map((n) => (
            <li key={n.id}>{n.title}</li>
          ))}
        </ul>
      );
  }
}
