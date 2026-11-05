// Kompacter flyout. Uses Tauri's global API (withGlobalTauri), no bundler needed.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWebview } = window.__TAURI__.webview;

const $ = (id) => document.getElementById(id);
const flyout = $("flyout");
const blob = $("blob");
let presets = [];
let file = null; // { path, info, estimate }

const fmt = (b) =>
  b >= 1 << 30 ? `${(b / (1 << 30)).toFixed(2)} GB`
  : b >= 1 << 20 ? `${(b / (1 << 20)).toFixed(1)} MB`
  : `${Math.round(b / 1024)} KB`;
const target = () => presets[$("preset").selectedIndex]?.bytes ?? 20 * (1 << 20);
const name = (p) => p.split(/[\\/]/).pop();

function show(state, title, detail, buttons = []) {
  flyout.dataset.state = state;
  $("title").textContent = title;
  $("detail").innerHTML = detail;
  for (const id of ["go", "cancel", "again"]) $(id).hidden = !buttons.includes(id);
  blob.style.removeProperty("--sx");
  blob.style.removeProperty("--sy");
}

function idle() {
  file = null;
  show("idle", "Drop a file", "It shrinks to fit and lands on your clipboard.");
}

async function open(path) {
  show("busy-probe", name(path), "Reading…");
  try {
    const r = await invoke("inspect", { path, target: target() });
    file = { path, ...r };
    const { info, estimate: e } = r;
    if (e.fits_as_is) {
      return show("done", "Already fits", `${fmt(info.size_bytes)} is under ${fmt(target())}.`, ["again"]);
    }
    let hint = "";
    if (e.video) {
      hint = e.video.fits
        ? `Compress makes it fit at ${e.video.height}p · ${e.video.quality} quality`
        : `<span class="bad">✗ Too long to fit. Trim it first.</span>`;
    }
    show("big", name(path),
      `${fmt(info.size_bytes)} · <span class="bad">✗ ${fmt(info.size_bytes - target())} over your limit</span><br>${hint}`,
      ["go"]);
  } catch (err) {
    show("big", name(path), `<span class="bad">✗ ${err}</span>`, ["again"]);
  }
}

async function compress() {
  if (!file) return;
  show("busy", "0%", "Compressing…", ["cancel"]);
  try {
    const r = await invoke("compress", { path: file.path, target: target(), keepQuality: false });
    const where = r.copied ? "Copied. Paste it anywhere." : `Saved to ${r.path}`;
    show("done", r.fits ? "✓ Done" : "✗ Still too big",
      `${fmt(r.original_bytes)} → <span class="${r.fits ? "ok" : "bad"}">${fmt(r.output_bytes)}</span><br>${where}`,
      ["again"]);
    blob.animate([{ transform: "scale(.2)" }, { transform: "scale(1.08)" }, { transform: "scale(1)" }],
      { duration: 420, easing: "cubic-bezier(.3,1.4,.5,1)" });
  } catch (err) {
    show("big", name(file.path), `<span class="bad">✗ ${err}</span>`, ["go", "again"]);
  }
}

listen("progress", ({ payload: p }) => {
  if (flyout.dataset.state !== "busy") return;
  $("title").textContent = `${Math.round(p * 100)}%`;
  flyout.style.setProperty("--p", p);
  // The press squashes the blob: flatter and a bit wider as progress goes up.
  blob.style.setProperty("--sy", (1 - 0.6 * p).toFixed(3));
  blob.style.setProperty("--sx", (1 + 0.15 * p).toFixed(3));
});

getCurrentWebview().onDragDropEvent(({ payload }) => {
  const busy = flyout.dataset.state.startsWith("busy");
  if (busy) return;
  if (payload.type === "over" || payload.type === "enter") flyout.dataset.state = "over";
  else if (payload.type === "leave") file ? open(file.path) : idle();
  else if (payload.type === "drop" && payload.paths?.length) open(payload.paths[0]);
});

$("go").onclick = compress;
$("cancel").onclick = () => invoke("cancel");
$("again").onclick = idle;
$("close").onclick = () => invoke("hide");
$("preset").onchange = () => file && open(file.path);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") invoke("hide");
});

(async () => {
  presets = await invoke("presets");
  $("preset").innerHTML = presets
    .map((p) => `<option>${p.label} · ${fmt(p.bytes)}</option>`)
    .join("");
  idle();
})();
