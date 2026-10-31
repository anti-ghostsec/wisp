// Hides the elements you've blocked on this site, and lets you pick new
// ones: right-click → Block element…, or Ctrl+Shift+E. Runs in the plugin's
// isolated world, so the page can't see or switch off the picker.
(() => {
  if (window.top !== window) return;
  const site = location.hostname.replace(/^www\./, "");
  let rules = [];

  // Pages with a strict Content-Security-Policy (style-src 'self') block
  // <style> elements, even in a shadow root; constructed sheets aren't.
  const sheet = (css) => {
    const s = new CSSStyleSheet();
    s.replaceSync(css);
    return s;
  };

  // ---- applying the rules ----
  const style = sheet("");
  const apply = () => {
    const hide = rules.map((r) => r.s).filter(valid);
    const unlock = rules.some((r) => r.unlock);
    style.replaceSync(
      (hide.length ? hide.join(",\n") + " { display: none !important; }\n" : "") +
        // A blocked overlay often leaves the page unscrollable behind it.
        (unlock ? "html, body { overflow: auto !important; }\n" : ""),
    );
    if (!document.adoptedStyleSheets.includes(style)) document.adoptedStyleSheets = [...document.adoptedStyleSheets, style];
  };
  const valid = (s) => {
    try {
      document.createDocumentFragment().querySelector(s);
      return true;
    } catch {
      return false;
    }
  };
  const load = async () => {
    const all = (await wisp.storage.get("rules")) || {};
    rules = all[site] || [];
    if (rules.length) apply();
  };
  const save = async (change) => {
    // Read again first: another tab may have added rules meanwhile.
    const all = (await wisp.storage.get("rules")) || {};
    all[site] = change(all[site] || []);
    if (!all[site].length) delete all[site];
    await wisp.storage.set("rules", all);
    rules = all[site] || [];
    apply();
  };
  load();
  // Pages that set their own adopted sheets would drop ours.
  document.addEventListener("DOMContentLoaded", () => rules.length && apply());

  // ---- picking ----
  let lastRightClicked = null;
  document.addEventListener("contextmenu", (e) => (lastRightClicked = e.target), true);

  const stableId = (id) => id.length < 40 && !/\d{3,}/.test(id) && !/^[:_-]|[:]/.test(id);
  // Generated class names (css-1x2y3z, sc-bdVaJa, _a8f3k2) change between visits.
  const stableClass = (c) => c.length < 40 && !(/\d/.test(c) && c.length > 5) && !/^(css|sc|jsx|emotion|styled)-/.test(c) && !/^_/.test(c);
  const count = (sel) => {
    try {
      return document.querySelectorAll(sel).length;
    } catch {
      return 0;
    }
  };
  // The shortest selector, built from the element up, that matches only it.
  const selectorFor = (el) => {
    const parts = [];
    for (let e = el; e && e.nodeType === 1 && e !== document.documentElement; e = e.parentElement) {
      if (e.id && stableId(e.id)) {
        parts.unshift("#" + CSS.escape(e.id));
        if (count(parts.join(" > ")) === 1) return parts.join(" > ");
        continue;
      }
      let part = e.localName;
      const cls = [...e.classList].filter(stableClass).slice(0, 3);
      part += cls.map((c) => "." + CSS.escape(c)).join("");
      const parent = e.parentElement;
      if (parent && [...parent.children].filter((s) => s !== e && s.matches(part)).length) {
        const same = [...parent.children].filter((s) => s.localName === e.localName);
        part += `:nth-of-type(${same.indexOf(e) + 1})`;
      }
      parts.unshift(part);
      if (count(parts.join(" > ")) === 1) return parts.join(" > ");
    }
    return parts.join(" > ");
  };

  let picker = null;

  const startPicker = (initial) => {
    if (picker) return;
    const host = document.createElement("wisp-element-picker");
    const root = host.attachShadow({ mode: "closed" });
    root.adoptedStyleSheets = [sheet(`
      :host { all: initial; }
      .box { position: fixed; pointer-events: none; z-index: 2147483647; background: #3a6ee833;
             outline: 2px solid #3a6ee8; border-radius: 2px; transition: all 60ms linear; }
      .panel { position: fixed; right: 16px; bottom: 16px; z-index: 2147483647; width: min(420px, calc(100vw - 32px));
               font: 13px system-ui, sans-serif; color: #eee; background: #242424f2; border-radius: 10px;
               box-shadow: 0 6px 24px #0008; padding: 12px; display: grid; gap: 8px; }
      .panel.left { right: auto; left: 16px; }
      b { font-weight: 600; }
      input { font: 12px ui-monospace, monospace; color: inherit; background: #0006; border: 1px solid #fff3;
              border-radius: 6px; padding: 6px 8px; width: 100%; box-sizing: border-box; }
      .row { display: flex; gap: 6px; align-items: center; }
      .grow { flex: 1; color: #aaa; }
      button { font: inherit; color: inherit; background: #fff2; border: 0; border-radius: 6px; padding: 5px 10px; cursor: pointer; }
      button:hover { background: #fff3; }
      button.main { background: #3a6ee8; color: #fff; }
      .hint { color: #999; font-size: 11.5px; }
    `)];
    root.innerHTML = `
    <div class="box"></div>
    <div class="panel">
      <div class="row"><b>Block element</b><span class="grow"></span></div>
      <input spellcheck="false" aria-label="CSS selector">
      <div class="row">
        <button class="up" title="Select the element around it (↑ or scroll up)">Bigger</button>
        <button class="down" title="Back to the smaller one (↓ or scroll down)">Smaller</button>
        <span class="grow"></span>
        <button class="cancel">Cancel</button><button class="main block">Block</button>
      </div>
      <div class="hint">Point at what to hide and click it. Scroll or ↑ ↓ to grow or shrink the selection, Esc to cancel.</div>
    </div>`;
    const $ = (s) => root.querySelector(s);
    const box = $(".box"), panel = $(".panel"), input = $("input"), info = $(".row .grow");
    document.documentElement.append(host);

    // `chain` is the path from what was pointed at up to the chosen element.
    // Frozen: the selection was chosen by hand (right-click, scroll, typing)
    // and stays until the pointer moves well away.
    let chain = [], level = 0, frozen = false, last = null, anchor = null;
    const freeze = () => {
      frozen = true;
      anchor = last;
    };
    const current = () => chain[level];
    const show = () => {
      const el = current();
      if (!el) {
        box.style.display = "none";
        return;
      }
      const r = el.getBoundingClientRect();
      Object.assign(box.style, { display: "block", left: r.left + "px", top: r.top + "px", width: r.width + "px", height: r.height + "px" });
      input.value = selectorFor(el);
      updateCount();
      // Keep the panel out of the way of the selection.
      panel.classList.toggle("left", r.right > innerWidth - 450 && r.bottom > innerHeight - 200 && r.left > 450);
    };
    const updateCount = () => {
      const n = count(input.value);
      info.textContent = n === 1 ? "1 element" : n + " elements";
    };
    const pointAt = (el) => {
      if (!el || el === host || el === document.documentElement) return;
      chain = [el];
      for (let e = el.parentElement; e && e !== document.documentElement; e = e.parentElement) chain.push(e);
      level = 0;
      show();
    };
    const grow = (by) => {
      level = Math.max(0, Math.min(chain.length - 1, level + by));
      freeze();
      show();
    };

    const inPanel = (e) => e.composedPath().includes(host);
    const onMove = (e) => {
      last = [e.clientX, e.clientY];
      if (frozen && (!anchor || Math.hypot(e.clientX - anchor[0], e.clientY - anchor[1]) > 40)) {
        if (anchor) frozen = false;
        else anchor = last;
      }
      if (inPanel(e) || frozen) return;
      pointAt(document.elementFromPoint(e.clientX, e.clientY));
    };
    // Swallow the page's own reaction to the click that picks.
    const swallow = (e) => {
      if (inPanel(e)) return;
      e.preventDefault();
      e.stopImmediatePropagation();
    };
    const onClick = (e) => {
      if (inPanel(e)) return;
      swallow(e);
      if (!frozen) pointAt(document.elementFromPoint(e.clientX, e.clientY));
      block();
    };
    const onWheel = (e) => {
      if (inPanel(e)) return;
      swallow(e);
      grow(e.deltaY < 0 ? 1 : -1);
    };
    const onKey = (e) => {
      if (e.key === "Escape") stop();
      else if (e.key === "ArrowUp" && e.target !== input) grow(1);
      else if (e.key === "ArrowDown" && e.target !== input) grow(-1);
      else if (e.key === "Enter") block();
      else if (inPanel(e)) return;
      else return;
      e.preventDefault();
      e.stopImmediatePropagation();
    };
    const listeners = [
      ["mousemove", onMove], ["click", onClick], ["mousedown", swallow], ["mouseup", swallow],
      ["pointerdown", swallow], ["pointerup", swallow], ["auxclick", swallow], ["wheel", onWheel, { passive: false }],
      ["keydown", onKey], ["scroll", show],
    ];
    for (const [type, fn, opts] of listeners) window.addEventListener(type, fn, { capture: true, ...opts });
    input.addEventListener("input", () => {
      freeze();
      updateCount();
      const first = count(input.value) ? document.querySelector(input.value) : null;
      if (first) {
        const r = first.getBoundingClientRect();
        Object.assign(box.style, { display: "block", left: r.left + "px", top: r.top + "px", width: r.width + "px", height: r.height + "px" });
      }
    });
    $(".up").onclick = () => grow(1);
    $(".down").onclick = () => grow(-1);
    $(".cancel").onclick = () => stop();
    $(".block").onclick = () => block();

    const stop = () => {
      for (const [type, fn, opts] of listeners) window.removeEventListener(type, fn, { capture: true, ...opts });
      host.remove();
      picker = null;
    };
    const block = async () => {
      const sel = input.value.trim();
      if (!sel || !count(sel)) return;
      const el = document.querySelector(sel);
      // Was it an overlay over the page? Then also let the page scroll again.
      const cs = getComputedStyle(el), r = el.getBoundingClientRect();
      const covers = (cs.position === "fixed" || cs.position === "sticky") && r.width * r.height > innerWidth * innerHeight * 0.3;
      const locked = [document.documentElement, document.body].some((e) => e && getComputedStyle(e).overflowY === "hidden");
      stop();
      const rule = { s: sel, unlock: covers && locked, added: Date.now() };
      await save((list) => [...list.filter((r) => r.s !== sel), rule]);
      undoToast(rule);
    };
    picker = { stop };
    if (initial) {
      pointAt(initial);
      freeze();
    }
    input.focus({ preventScroll: true });
  };

  const undoToast = (rule) => {
    const host = document.createElement("wisp-element-picker");
    const root = host.attachShadow({ mode: "closed" });
    root.adoptedStyleSheets = [sheet(`
      div { position: fixed; right: 16px; bottom: 16px; z-index: 2147483647; font: 13px system-ui, sans-serif; color: #eee;
            background: #242424f2; border-radius: 10px; box-shadow: 0 6px 24px #0008; padding: 10px 12px;
            display: flex; gap: 12px; align-items: center; }
      button { font: inherit; color: #8ab4ff; background: none; border: 0; cursor: pointer; padding: 0; }
    `)];
    root.innerHTML = `<div><span></span><button>Undo</button></div>`;
    root.querySelector("span").textContent = "Blocked on " + site;
    root.querySelector("button").onclick = async () => {
      host.remove();
      await save((list) => list.filter((r) => r.s !== rule.s));
    };
    document.documentElement.append(host);
    setTimeout(() => host.remove(), 6000);
  };

  wisp.on("context-menu", ({ id }) => {
    if (id === "block") startPicker(lastRightClicked && lastRightClicked.nodeType === 1 ? lastRightClicked : lastRightClicked?.parentElement);
  });
  wisp.on("action", ({ id }) => {
    if (id === "pick") picker ? picker.stop() : startPicker(null);
  });
})();
