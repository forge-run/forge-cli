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
        </main>
      );
    case "failed":
      // `hello` is declared `authenticated`: a visitor who has not signed in
      // reaches this arm, and so does any other failure, named by its code.
      return (
        <main>
          <p>{hello.code === "UNAUTHENTICATED" ? "Sign in to be greeted." : hello.message}</p>
        </main>
      );
  }
}
