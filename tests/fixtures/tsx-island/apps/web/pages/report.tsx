import { Node } from "forge";
import Chart from "../components/chart";

export default function ReportPage(): Node {
  return (
    <main>
      <h1>Report</h1>
      <Chart title="Weekly" points={[3, 5, 8]} />
    </main>
  );
}
