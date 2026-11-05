// The flyout window is a fixed transparent canvas (see tauri.conf.json). The card inside it
// is anchored to the bottom edge and changes height by itself, so the OS window never
// resizes mid-animation. Clicks outside the card pass through (src-tauri clickthrough.rs).
import { invoke } from "@tauri-apps/api/core";
import { closeEase, Spring } from "./spring";

/** Gap between the card and the window edge, room for the card's shadow. */
export const INSET = 12;
export const CARD_W = 360;
export const BASE_H = 472;
/** Height the card starts from when it opens: a small pill at the bottom. */
const PILL_H = 56;
const CLOSE_MS = 340;

const inTauri = "__TAURI_INTERNALS__" in window;
const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

/** Tallest the card may get: what fits in the window. */
export const maxCardH = () => window.innerHeight - INSET * 2;

class Card {
  private el: HTMLElement | null = null;
  private spring = new Spring(BASE_H);
  private raf = 0;
  private last = 0;
  private closing: { from: number; start: number; done: (closed: boolean) => void } | null = null;
  private wanted = BASE_H;
  /** Height last drawn, so a reopen mid-close starts from where the card is. */
  private shown = BASE_H;

  /** True while the card is folding away. */
  get isClosing() {
    return this.closing !== null;
  }

  attach(el: HTMLElement) {
    this.el = el;
    // In the app the card starts folded away and opens when the tray shows the window,
    // so it never flashes at full size first. In a plain browser (UI dev) it's just open.
    if (inTauri) el.classList.add("closed");
    this.paint(this.spring.value);
  }

  /** Target height for the current view. Springs there unless the card is closed. */
  setTarget(h: number) {
    this.wanted = Math.min(h, maxCardH());
    this.el?.style.setProperty("--content-h", `${this.wanted}px`);
    if (this.closing) return;
    this.spring.configure(0.42, 0.8);
    this.go(this.wanted);
  }

  /** Grow open from a pill at the bottom edge, like a macOS panel. */
  open() {
    const wasClosed = !this.closing;
    if (this.closing) {
      // Reopened mid-close: settle the old close and grow back from the current height.
      this.closing.done(false);
      this.closing = null;
      this.el?.classList.remove("closing");
    }
    this.el?.classList.remove("closed");
    if (reduced()) return this.jump(this.wanted);
    this.spring.set(wasClosed ? PILL_H : this.shown);
    this.spring.configure(0.5, 0.72);
    this.go(this.wanted);
  }

  /** Fold down into the bottom edge. Resolves true once it's gone, or false if it was
   *  reopened (or was already closing) before the fold finished. */
  close(): Promise<boolean> {
    if (this.closing) return Promise.resolve(false); // already on its way
    if (reduced() || !this.el) {
      this.el?.classList.add("closed");
      return Promise.resolve(true);
    }
    return new Promise((done) => {
      this.closing = { from: this.spring.value, start: performance.now(), done };
      this.el!.classList.add("closing");
      this.loop();
    });
  }

  private jump(h: number) {
    this.spring.set(h);
    this.paint(h);
    this.report();
  }

  private go(h: number) {
    this.spring.target = h;
    // Let clicks reach the whole area the card will cover while it moves.
    this.report(Math.max(this.spring.value, h));
    this.loop();
  }

  private loop() {
    if (this.raf) return;
    this.last = performance.now();
    const tick = (now: number) => {
      const dt = Math.min(0.05, (now - this.last) / 1000);
      this.last = now;
      if (this.closing) {
        const t = Math.min(1, (now - this.closing.start) / CLOSE_MS);
        const h = this.closing.from + (0 - this.closing.from) * closeEase(t);
        this.paint(Math.max(0, h), 1 - t);
        if (t >= 1) {
          const done = this.closing.done;
          this.closing = null;
          this.raf = 0;
          this.el?.classList.remove("closing");
          this.el?.classList.add("closed");
          this.spring.set(this.wanted);
          return done(true);
        }
      } else {
        this.spring.step(dt);
        this.paint(this.spring.value);
        if (this.spring.settled) {
          this.spring.set(this.spring.target);
          this.paint(this.spring.value);
          this.report();
          this.raf = 0;
          return;
        }
      }
      this.raf = requestAnimationFrame(tick);
    };
    this.raf = requestAnimationFrame(tick);
  }

  /** One style write per frame; height changes clip the content from the top. */
  private paint(h: number, opacity = 1) {
    if (!this.el) return;
    this.shown = h;
    this.el.style.height = `${h}px`;
    this.el.style.opacity = opacity < 1 ? String(Math.max(0, opacity * 1.4)) : "";
  }

  /** Tell Rust where the card is, for click-through. */
  private report(h = this.spring.value) {
    if (!inTauri) return;
    const top = window.innerHeight - INSET - h;
    invoke("set_hit_rect", { x: INSET, y: top, w: CARD_W, h }).catch(() => {});
  }
}

export const card = new Card();
