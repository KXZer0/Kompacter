// Spring motion for the flyout card. `response` ≈ seconds to settle, `damping` < 1 adds a
// small overshoot. Runs on a fixed 120 Hz clock with an accumulator, so frame drops or a
// slow first frame never change how the motion looks.

const STEP = 1 / 120;

export class Spring {
  value: number;
  target: number;
  velocity = 0;
  private stiffness = 0;
  private friction = 0;
  private carry = 0;

  constructor(value: number, response = 0.5, damping = 0.72) {
    this.value = this.target = value;
    this.configure(response, damping);
  }

  configure(response: number, damping: number) {
    const w = (2 * Math.PI) / response;
    this.stiffness = w * w;
    this.friction = 2 * damping * w;
  }

  /** Jump straight to `v`, no motion. */
  set(v: number) {
    this.value = this.target = v;
    this.velocity = 0;
    this.carry = 0;
  }

  get settled() {
    return Math.abs(this.target - this.value) < 0.3 && Math.abs(this.velocity) < 1;
  }

  step(dt: number) {
    this.carry += dt;
    while (this.carry >= STEP) {
      const force = this.stiffness * (this.target - this.value) - this.friction * this.velocity;
      this.velocity += force * STEP;
      this.value += this.velocity * STEP;
      this.carry -= STEP;
    }
  }
}

/** Close curve: eases in and out, no bounce. */
export const closeEase = (t: number) => (t < 0.5 ? 8 * t ** 4 : 1 - (-2 * t + 2) ** 4 / 2);
