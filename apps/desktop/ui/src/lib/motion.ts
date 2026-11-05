// Motion helpers shared by the views. Everything animates transform/opacity only.

const rm = matchMedia("(prefers-reduced-motion: reduce)");
export const reduced = () => rm.matches;

/** The snappy spring used everywhere: ~260 ms, ~4% overshoot. Falls back to a cubic-bezier. */
export const SPRING = CSS.supports("transition-timing-function", "linear(0, 1)")
  ? "linear(0, .16, .44 12.5%, .7 20%, .88 27.5%, .98 35%, 1.03 42.5%, 1.045 50%, 1.04 60%, 1.017 72.5%, 1.003 85%, 1)"
  : "cubic-bezier(.2,.9,.25,1.05)";

/** FLIP: animate `el` from a previous rect to where it is now (scale + translate). */
export function flipFrom(el: Element, from: DOMRect | null, origin = "50% 50%") {
  if (!from || reduced()) return;
  const to = el.getBoundingClientRect();
  if (!to.width || !to.height) return;
  if (Math.abs(to.width - from.width) < 1 && Math.abs(to.height - from.height) < 1) return;
  (el as HTMLElement).style.transformOrigin = origin;
  el.animate(
    [{ transform: `scale(${from.width / to.width},${from.height / to.height})` }, { transform: "none" }],
    { duration: 280, easing: SPRING },
  );
}

type Key = "y" | "sx" | "sy";

/**
 * A tiny stiff spring (k=700, ζ≈0.64) writing one transform. Used for the blob's squish,
 * bounce and drag-over swell. Sleeps when settled, so it costs nothing at rest.
 */
export class Spring {
  v: Record<Key, number> = { y: 0, sx: 1, sy: 1 };
  t: Record<Key, number> = { y: 0, sx: 1, sy: 1 };
  vel: Record<Key, number> = { y: 0, sx: 0, sy: 0 };
  private raf = 0;
  private last = 0;

  constructor(private el: HTMLElement, private shadow?: HTMLElement, private k = 700, private c = 34) {}

  to(t: Partial<Record<Key, number>>) {
    Object.assign(this.t, t);
    this.kick();
  }

  push(v: Partial<Record<Key, number>>) {
    for (const k in v) this.vel[k as Key] += v[k as Key]!;
    this.kick();
  }

  stop() {
    cancelAnimationFrame(this.raf);
    this.raf = 0;
  }

  private kick() {
    if (reduced()) {
      Object.assign(this.v, this.t);
      this.vel = { y: 0, sx: 0, sy: 0 };
      this.apply();
      return;
    }
    if (!this.raf) {
      this.last = performance.now();
      this.raf = requestAnimationFrame(this.step);
    }
  }

  private step = (now: number) => {
    const dt = Math.min(0.05, (now - this.last) / 1000);
    this.last = now;
    const n = 4, h = dt / n;
    let moving = false;
    for (let i = 0; i < n; i++)
      for (const k of ["y", "sx", "sy"] as Key[]) {
        const a = -this.k * (this.v[k] - this.t[k]) - this.c * this.vel[k];
        this.vel[k] += a * h;
        this.v[k] += this.vel[k] * h;
      }
    for (const k of ["y", "sx", "sy"] as Key[])
      if (Math.abs(this.v[k] - this.t[k]) > 5e-4 || Math.abs(this.vel[k]) > 0.01) moving = true;
    if (!moving) Object.assign(this.v, this.t);
    this.apply();
    this.raf = moving ? requestAnimationFrame(this.step) : 0;
  };

  private apply() {
    const { y, sx, sy } = this.v;
    this.el.style.transform = `translateY(${y.toFixed(2)}px) scale(${sx.toFixed(4)},${sy.toFixed(4)})`;
    if (this.shadow) {
      const s = Math.max(0.5, 1 + y / 120);
      this.shadow.style.transform = `scale(${s.toFixed(3)})`;
      this.shadow.style.opacity = s.toFixed(2);
    }
  }
}
