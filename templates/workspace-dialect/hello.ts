/** The `{{domain}}::hello` starter op. */
import { op, OpContext } from "forge";
import { HelloRequest, HelloResponse } from "forge/schema";

export const hello = op((ctx: OpContext, req: HelloRequest): HelloResponse => {
  return { message: "hello, " + req.name + ", from {{domain}}" };
});
