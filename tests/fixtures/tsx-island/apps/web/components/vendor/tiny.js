// A copied-in third-party file is the customer's code to forge.
export function line(canvas, points) {
  const g = canvas.getContext("2d");
  points.forEach((y, x) => g.lineTo(x * 10, 100 - y));
  g.stroke();
}
