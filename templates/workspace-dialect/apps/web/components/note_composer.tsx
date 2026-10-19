import { Node, action, isPending } from "forge";

/**
 * The form that adds a note. While `add_note` is in flight its button is
 * disabled and reads "Saving…"; the live list shows the note once it lands.
 */
export default function NoteComposer(): Node {
  const saving = isPending(action("add_note"));
  return (
    <form method="post" action={action("add_note")}>
      <input id="note-body" type="text" name="body" required />
      <button id="note-add" type="submit" disabled={saving}>
        {saving ? "Saving…" : "Add"}
      </button>
    </form>
  );
}
