/** SPA-6: reads no table, so no change could make a load of it re-run. */
import { op, value, OpContext, Value } from "forge";

export const note_clock = op("note_clock", (ctx: OpContext, input: Value) => {
  return value({ count: 0 });
});
