import { Island, Node } from "forge";

export interface ChartProps {
  title: string;
  points: number[];
}

export default function Chart(props: ChartProps): Node {
  return (
    <figure>
      <figcaption>{props.title}</figcaption>
      <Island src="./chart.island.js" props={{ title: props.title, points: props.points, animate: true }} />
    </figure>
  );
}
