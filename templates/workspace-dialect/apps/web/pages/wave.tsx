// The page at `/wave`, a client page: `mode = "client"` serves it as a shell
// the browser renders, so its components may hold state cells and click
// handlers. Its loader is the same as a server page's, but the browser runs
// it, fetching `hello` from the op's REST route as the signed-in user.
import { Node, Loaded, load } from "forge";
import { HelloResponse } from "forge/schema";
import Greeting from "../components/greeting";
import Wave from "../components/wave";

export const mode = "client";

export const loader = {
  hello: load("hello", { name: "{{workspace_name}}" }),
};

export interface WavePageProps {
  hello: Loaded<HelloResponse>;
}

export default function WavePage({ hello }: WavePageProps): Node {
  switch (hello.kind) {
    case "ok":
      return (
        <main>
          <Greeting message={hello.value.message} />
          <Wave label="Wave" />
        </main>
      );
    case "failed":
      return (
        <main>
          <p>{hello.code === "UNAUTHENTICATED" ? "Sign in to wave." : hello.message}</p>
        </main>
      );
  }
}
