// The flyout window is 360×472 by default. Settings and the trimmer grow it upward
// (it sits above the taskbar), keeping the bottom edge where it was.
import { getCurrentWindow, LogicalSize, PhysicalPosition } from "@tauri-apps/api/window";

export const WIDTH = 360;
export const BASE_HEIGHT = 472;

const inTauri = "__TAURI_INTERNALS__" in window;
let current = BASE_HEIGHT;
let queue: Promise<void> = Promise.resolve();

/** Resize the window to `h` logical px, anchored at its bottom edge. Calls are serialized. */
export function setHeight(h: number): Promise<void> {
  if (!inTauri || h === current) return queue;
  current = h;
  queue = queue.then(async () => {
    const w = getCurrentWindow();
    const [scale, pos, size] = await Promise.all([w.scaleFactor(), w.outerPosition(), w.outerSize()]);
    const bottom = pos.y + size.height;
    const newH = Math.round(h * scale);
    // Sent back to back so the move and resize land in the same frame.
    await Promise.all([
      w.setPosition(new PhysicalPosition(pos.x, bottom - newH)),
      w.setSize(new LogicalSize(WIDTH, h)),
    ]);
  }).catch(() => {});
  return queue;
}

/** Usable height above the taskbar, so a tall view never runs off-screen. */
export const maxHeight = () => Math.max(BASE_HEIGHT, Math.min(640, screen.availHeight - 24));
