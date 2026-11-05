// One polite live region for the whole flyout (progress, results, errors).

export const live = $state({ text: "" });

let timer = 0;
/** Announce a message to screen readers. Re-announces even if the text repeats. */
export function say(msg: string) {
  clearTimeout(timer);
  live.text = "";
  timer = window.setTimeout(() => (live.text = msg), 50);
}
