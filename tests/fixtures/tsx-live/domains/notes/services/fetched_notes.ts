/**
 * SPA-6: reads `notes` and the world beyond the workspace, which no stream
 * follows, so its reads are wide.
 */
import { http, op, storage, value, OpContext, Value } from "forge";

export const fetched_notes = op("fetched_notes", (ctx: OpContext, input: Value) => {
  const answered = http.fetch("GET", "https://api.example.com/v1/ping", [], null);
  const found = storage.query({ from: "notes", limit: 20 });
  return value({ items: storage.rows(found), status: answered.get("status", "") });
});
