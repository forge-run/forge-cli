import { Node } from "forge";
import Chart from "../components/chart";

export const mode = "client";

export default function LivePage(): Node {
  return (
    <main>
      <h1>Live</h1>
      <Chart title="Today" points={[1, 2]} />
    </main>
  );
}
