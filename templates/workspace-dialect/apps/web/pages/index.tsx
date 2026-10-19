// The page at `/`. Its data comes only from the ops its loader names: the
// host runs each as the requesting user and hands the answer to the prop of
// the same name, typed by the op's generated `<Op>Response`.
import { Node, Loaded, load } from "forge";
import { HelloResponse } from "forge/schema";
import Greeting from "../components/greeting";

export const loader = {
  hello: load("hello", { name: "{{workspace_name}}" }),
};

export interface HomeProps {
  hello: Loaded<HelloResponse>;
}

export default function Home({ hello }: HomeProps): Node {
  switch (hello.kind) {
    case "ok":
      return (
        <main>
          <Greeting message={hello.value.message} />
          <p>
            <a href="/notes">Notes</a> and <a href="/wave">Wave</a> are client pages.
          </p>
        </main>
      );
    case "failed":
      // `hello` is declared `authenticated`: a visitor who has not signed in
      // reaches this arm as `denied`, and so does any other failure, named by
      // its reason and the trace id an operator finds its cause under. The
      // op's own message is in that trace, never in the page.
      return (
        <main>
          <p>{hello.reason === "denied" ? "Sign in to be greeted." : `The greeting did not load (trace ${hello.traceId}).`}</p>
        </main>
      );
  }
}
