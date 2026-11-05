// A SwiftUI-style spring: `response` is roughly the time to settle (s), `damping` < 1 gives a
// little overshoot. Integrated in small steps so a dropped frame never makes it jump.
// Same model Coucou uses for its island (MIT, github.com/Louis-CFM/coucou, src/core/anim.ts).

export class Spring {
  value: number;
  target: number;
  velocity = 0;
  private omega: number;
  private zeta: number;

  constructor(value: number, response = 0.5, damping = 0.72) {
    this.value = value;
    this.target = value;
    this.omega = (2 * Math.PI) / response;
    this.zeta = damping;
  }

  configure(response: number, damping: number) {
    this.omega = (2 * Math.PI) / response;
    this.zeta = damping;
  }

  /** Jump straight to `v`, no motion. */
  set(v: number) {
    this.value = this.target = v;
    this.velocity = 0;
  }

  get settled() {
    return Math.abs(this.target - this.value) < 0.3 && Math.abs(this.velocity) < 1;
  }

  step(dt: number) {
    const steps = Math.max(1, Math.ceil(dt / (1 / 240)));
    const h = dt / steps;
    for (let i = 0; i < steps; i++) {
      const acc = this.omega * this.omega * (this.target - this.value) - 2 * this.zeta * this.omega * this.velocity;
      this.velocity += acc * h;
      this.value += this.velocity * h;
    }
  }
}

/** Close curve: quick and calm, no bounce (cubic-bezier(.45,0,.2,1), 340 ms). */
export function closeEase(t: number) {
  // Bisection on the bezier's x; 12 rounds is plenty at 60 fps.
  const [x1, y1, x2, y2] = [0.45, 0, 0.2, 1];
  const bx = (u: number) => 3 * (1 - u) ** 2 * u * x1 + 3 * (1 - u) * u * u * x2 + u ** 3;
  const by = (u: number) => 3 * (1 - u) ** 2 * u * y1 + 3 * (1 - u) * u * u * y2 + u ** 3;
  let lo = 0, hi = 1, u = t;
  for (let i = 0; i < 12; i++) {
    if (bx(u) < t) lo = u;
    else hi = u;
    u = (lo + hi) / 2;
  }
  return by(u);
}
