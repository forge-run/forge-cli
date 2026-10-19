/** SPA-6: reads `tags`, which the test's `schema.lock` declares or leaves out. */
import { op, storage, value, OpContext, Value } from "forge";

export const list_tags = op("list_tags", (ctx: OpContext, input: Value) => {
  const found = storage.query({ from: "tags", limit: 20 });
  return value({ items: storage.rows(found) });
});
