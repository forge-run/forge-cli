/** SPA-6: reads `notes` and nothing else, so a load of it may be live. */
import { op, storage, value, OpContext, Value } from "forge";

export const list_notes = op("list_notes", (ctx: OpContext, input: Value) => {
  const found = storage.query({ from: "notes", limit: 20 });
  return value({ items: storage.rows(found) });
});
