// Renders list.html: the elements blocked on each site. Edit a selector in
// place (saved when you leave the field) or remove it.
(() => {
  const $ = (s) => document.querySelector(s);
  let all = {};

  const valid = (s) => {
    try {
      document.createDocumentFragment().querySelector(s);
      return true;
    } catch {
      return false;
    }
  };
  const save = async () => {
    for (const site of Object.keys(all)) if (!all[site].length) delete all[site];
    await wisp.storage.set("rules", all);
  };
  const el = (tag, props = {}, ...children) => {
    const e = Object.assign(document.createElement(tag), props);
    e.append(...children);
    return e;
  };

  const render = () => {
    const q = $("#q").value.trim().toLowerCase();
    const sites = Object.keys(all).sort().filter((s) => !q || s.includes(q) || all[s].some((r) => r.s.toLowerCase().includes(q)));
    const total = Object.values(all).reduce((n, l) => n + l.length, 0);
    $("#status").textContent = total
      ? `${total} blocked element${total === 1 ? "" : "s"} on ${Object.keys(all).length} site${Object.keys(all).length === 1 ? "" : "s"}. Changes apply when a page loads.`
      : "Nothing blocked yet. Right-click something on a page and choose Block element…";
    const box = $("#sites");
    box.replaceChildren();
    for (const site of sites) {
      const clear = el("button", { textContent: "Remove all", title: "Unblock everything on " + site });
      clear.onclick = async () => {
        delete all[site];
        await save();
        render();
      };
      const ul = el("ul");
      for (const rule of all[site]) {
        const input = el("input", { value: rule.s, spellcheck: false, title: "CSS selector" });
        input.onchange = async () => {
          const s = input.value.trim();
          input.classList.toggle("bad", !valid(s));
          if (!s || !valid(s)) return;
          rule.s = s;
          await save();
        };
        const when = el("small", { textContent: rule.added ? new Date(rule.added).toLocaleDateString() : "" });
        const remove = el("button", { textContent: "✕", title: "Unblock" });
        remove.onclick = async () => {
          all[site] = all[site].filter((r) => r !== rule);
          await save();
          render();
        };
        ul.append(el("li", {}, input, when, remove));
      }
      box.append(el("h2", {}, el("span", { textContent: site }), clear), ul);
    }
  };

  $("#q").oninput = render;
  (async () => {
    all = (await wisp.storage.get("rules")) || {};
    render();
  })();
})();
