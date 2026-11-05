// One tooltip for the whole app, outside React (§9.2). The `title`
// attributes stay the source of truth: on hover the text moves to
// `data-tip` (so the webview does not show its own: late, unstyled, in the
// platform's font, slow on Windows) and a styled one appears under the
// element after a short delay — at once when another was open a moment
// ago, as a toolbar is read button by button. A press, a key, a scroll or
// leaving the element hides it. Keyboard focus shows it too.

const DELAY_MS = 500;
/// Within this time after a tooltip closed, the next one opens at once.
const INSTANT_WINDOW_MS = 400;
const MARGIN = 8;
const GAP = 6;

export function installTooltips(): void {
  const tip = document.createElement("div");
  tip.className = "tip";
  tip.setAttribute("role", "tooltip");
  tip.hidden = true;
  document.body.append(tip);

  let anchor: HTMLElement | null = null;
  let timer = 0;
  let hiddenAt = 0;

  const textOf = (el: HTMLElement): string => {
    const title = el.getAttribute("title");
    if (title !== null) {
      // The name for assistive technology stays, as the title gave it.
      if (!el.hasAttribute("aria-label") && !el.hasAttribute("aria-labelledby")) {
        el.setAttribute("aria-label", title);
      }
      el.dataset.tip = title;
      el.removeAttribute("title");
    }
    return el.dataset.tip ?? "";
  };

  const place = () => {
    if (!anchor) return;
    const a = anchor.getBoundingClientRect();
    const t = tip.getBoundingClientRect();
    const below = a.bottom + GAP + t.height <= window.innerHeight - MARGIN;
    const top = below ? a.bottom + GAP : a.top - GAP - t.height;
    const left = Math.max(MARGIN, Math.min(a.left + a.width / 2 - t.width / 2, window.innerWidth - MARGIN - t.width));
    tip.style.top = `${Math.round(top)}px`;
    tip.style.left = `${Math.round(left)}px`;
    tip.dataset.side = below ? "below" : "above";
  };

  const show = () => {
    if (!anchor) return;
    const text = anchor.dataset.tip;
    if (!text) return;
    tip.textContent = text;
    tip.hidden = false;
    if (Date.now() - hiddenAt < INSTANT_WINDOW_MS) tip.dataset.instant = "";
    else delete tip.dataset.instant;
    place();
    // Next frame: the transition starts from the hidden state.
    tip.dataset.open = "";
  };

  const hide = () => {
    window.clearTimeout(timer);
    timer = 0;
    if (!tip.hidden) {
      hiddenAt = Date.now();
      tip.hidden = true;
      delete tip.dataset.open;
    }
    anchor = null;
  };

  const arm = (el: HTMLElement, instant: boolean) => {
    if (el === anchor) return;
    hide();
    if (textOf(el) === "") return;
    anchor = el;
    if (instant || Date.now() - hiddenAt < INSTANT_WINDOW_MS) show();
    else timer = window.setTimeout(show, DELAY_MS);
  };

  document.addEventListener("pointerover", (e) => {
    const el = (e.target as Element | null)?.closest?.("[title], [data-tip]");
    if (el instanceof HTMLElement && !tip.contains(el)) arm(el, false);
  });
  document.addEventListener("pointerout", (e) => {
    if (!anchor) return;
    const to = e.relatedTarget as Node | null;
    if (!to || !anchor.contains(to)) hide();
  });
  document.addEventListener("focusin", (e) => {
    const el = e.target as HTMLElement | null;
    if (el instanceof HTMLElement && el.matches(":focus-visible") && (el.hasAttribute("title") || el.dataset.tip)) {
      arm(el, true);
    }
  });
  document.addEventListener("focusout", hide);
  document.addEventListener("pointerdown", hide, true);
  document.addEventListener("keydown", hide, true);
  document.addEventListener("scroll", hide, true);
  window.addEventListener("blur", hide);
}
