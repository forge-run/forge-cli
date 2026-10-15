import { Node, useState } from "forge";

export interface WaveProps {
  label: string;
}

/**
 * A button that counts its clicks. `waves` is a state cell: it lives in the
 * browser, so this component renders only on a client page. A click's one
 * effect is setting the cell, and the page re-renders from it.
 */
export default function Wave({ label }: WaveProps): Node {
  const [waves, setWaves] = useState<number>(0);
  return (
    <section>
      <button onClick={() => setWaves(waves + 1)}>{label}</button>
      <p>{`Waves: ${waves}`}</p>
    </section>
  );
}
