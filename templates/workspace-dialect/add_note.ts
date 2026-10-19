/** The `{{domain}}::add_note` op: write one note as the caller. */
import { op, OpContext } from "forge";
import { AddNoteRequest, AddNoteResponse } from "forge/schema";
import { Notes } from "forge/schema/tables";

export const addNote = op((ctx: OpContext, req: AddNoteRequest): AddNoteResponse => {
  const _written = Notes.insert([{ body: req.body }]).execute();
  return { body: req.body };
});
