"use client";
// The page at `/wave`, a client page: `"use client"` serves it as a shell
// the browser renders, so its components may hold state cells and click
// handlers. Its loader is the same as a server page's, but the browser runs
// it, fetching `hello` from the op's REST route as the signed-in user. It
// sits in the app's layout, which already holds the `<main>`.
import { Node, Loaded, load } from "forge";
import { HelloResponse } from "forge/schema";
import Greeting from "../components/greeting";
import Wave from "../components/wave";
import AppLayout from "../layouts/app";

export const layout = AppLayout;

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
        <section>
          <Greeting message={hello.value.message} />
          <Wave label="Wave" />
        </section>
      );
    case "failed":
      return (
        <section>
          <p>{hello.reason === "denied" ? "Sign in to wave." : `The greeting did not load (trace ${hello.traceId}).`}</p>
        </section>
      );
  }
}
