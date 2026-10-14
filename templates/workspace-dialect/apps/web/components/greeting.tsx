import { Node } from "forge";

export interface GreetingProps {
  message: string;
}

/** The `hello` op's answer, as the page's heading. */
export default function Greeting({ message }: GreetingProps): Node {
  return <h1>{message}</h1>;
}
