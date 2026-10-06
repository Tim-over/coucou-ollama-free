// The "ghost" Mochi that follows the cursor while you throw it at a window.
// It lives in its own small, transparent, click-through window; Rust moves that
// window to the cursor each tick, and this page just keeps Mochi animated.
//
// It is the same character as the island's, drawn small on a transparent canvas.

import { BotEngine } from "../mochi/engine";

const SIZE = 64; // logical px of Mochi
const OVERHANG = 34; // room above for particles / squash, like the island

const canvas = document.getElementById("ghost-canvas") as HTMLCanvasElement;
const ctx = canvas.getContext("2d");
const engine = new BotEngine();

const dpr = Math.min(2, window.devicePixelRatio || 1);
const hCss = SIZE + OVERHANG;
canvas.width = Math.round(SIZE * dpr);
canvas.height = Math.round(hCss * dpr);
canvas.style.width = `${SIZE}px`;
canvas.style.height = `${hCss}px`;

// Picked-up look: wide surprised eyes and a lively sway.
engine.particleOverhang = OVERHANG;
engine.setState("working");
engine.grab();

let last = performance.now();
function frame(nowMs: number) {
  const dt = Math.min(0.05, (nowMs - last) / 1000);
  last = nowMs;
  // A gentle eye drift so it feels alive while being carried.
  engine.lookX = Math.sin(nowMs / 400) * 0.5;
  engine.lookY = Math.cos(nowMs / 500) * 0.3;
  engine.update(dt);
  if (ctx) {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, SIZE, hCss);
    engine.draw(ctx, SIZE, hCss);
  }
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);

// A fresh startle each time the window is shown again.
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) engine.grab();
});
