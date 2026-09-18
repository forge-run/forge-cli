import { op, OpContext, Value } from "forge";

interface Greeting {
  message: string;
}

export const hello = op("hello", (ctx: OpContext, input: Value) => {
  const name: string = input.get("name", "world");
  const out: Greeting = { message: "hello, " + name + ", from {{domain}}" };
  return out;
});
