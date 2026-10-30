// Inverts light pages only. The page is darkened right away (no white flash),
// then checked once it has drawn: if it's already dark, the inversion is
// removed, and it's checked again whenever the site switches theme.
(() => {
  const root = () => document.documentElement;
  const darken = (on) => root()?.classList.toggle("wisp-dark", on);

  // Relative luminance of a CSS color, or null if it's (nearly) transparent.
  const luminance = (color) => {
    const m = color.match(/[\d.]+/g);
    if (!m || m.length < 3) return null;
    const [r, g, b, a = 1] = m.map(Number);
    if (a < 0.5) return null;
    const lin = (c) => ((c /= 255) <= 0.04 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
  };

  // The first opaque background behind a point of the page. The page's
  // canvas takes the body's background when the root has none.
  const backgroundAt = (x, y) => {
    for (let el = document.elementFromPoint(x, y); el; el = el.parentElement) {
      const l = luminance(getComputedStyle(el).backgroundColor);
      if (l !== null) return l;
      if (el === root() && document.body) return luminance(getComputedStyle(document.body).backgroundColor);
    }
    return null;
  };

  const alreadyDark = () => {
    const w = innerWidth, h = innerHeight;
    const samples = [[w / 2, h / 2], [w / 4, h / 3], [(3 * w) / 4, (2 * h) / 3], [w / 2, h - 5]]
      .map(([x, y]) => backgroundAt(x, y))
      .filter((l) => l !== null);
    if (samples.length) return samples.filter((l) => l < 0.2).length > samples.length / 2;
    // Nothing opaque: the canvas color decides. Dark text on it means light.
    const scheme = getComputedStyle(root()).colorScheme;
    if (/dark/.test(scheme) && (!/light/.test(scheme) || matchMedia("(prefers-color-scheme: dark)").matches)) return true;
    const text = document.body && luminance(getComputedStyle(document.body).color);
    return text !== null && text > 0.5;
  };

  // Measured without our own styles (they give the root a white background).
  // Removing and re-adding the class in one go never paints in between.
  const check = () => {
    if (!document.body) return;
    darken(false);
    darken(!alreadyDark());
  };

  if (root()) darken(true);
  else new MutationObserver((_, o) => root() && (darken(true), o.disconnect())).observe(document, { childList: true });

  document.addEventListener("DOMContentLoaded", check);
  addEventListener("load", check);
  // Sites that load their theme with scripts, or switch it later.
  for (const ms of [500, 1500, 4000]) setTimeout(check, ms);
  let queued = false;
  const recheck = () => {
    if (queued) return;
    queued = true;
    setTimeout(() => { queued = false; check(); }, 150);
  };
  addEventListener("DOMContentLoaded", () => {
    const opts = { attributes: true, attributeFilter: ["class", "style", "data-theme", "data-color-mode"] };
    new MutationObserver((records) => {
      // Ignore our own class change.
      if (records.some((r) => r.target !== root() || r.attributeName !== "class" || !onlyOurs(r))) recheck();
    }).observe(root(), { ...opts, attributeOldValue: true });
    if (document.body) new MutationObserver(recheck).observe(document.body, opts);
  });
  const onlyOurs = (r) => {
    const before = new Set((r.oldValue || "").split(/\s+/).filter(Boolean));
    const after = new Set([...root().classList]);
    before.delete("wisp-dark");
    after.delete("wisp-dark");
    return before.size === after.size && [...after].every((c) => before.has(c));
  };
})();
