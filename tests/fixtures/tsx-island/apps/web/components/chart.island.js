// An island: plain ES modules, served as pushed. `forge.call` is the only
// platform handle; the session never reaches it.
import { line } from "./vendor/tiny.js";

export default function mount(element, props, forge) {
  const canvas = document.createElement("canvas");
  element.append(canvas);
  line(canvas, props.points);
  canvas.addEventListener("click", async () => {
    await forge.call("notes", "create_note", { title: props.title });
  });
}
