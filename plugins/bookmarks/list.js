// Renders list.html (runs in the plugin's isolated world, so it has the wisp API).
(() => {
  const $ = (s) => document.querySelector(s);
  let all = [];

  const render = () => {
    const q = $("#q").value.trim().toLowerCase();
    const shown = all.filter((b) => !q || (b.name + " " + b.url).toLowerCase().includes(q));
    const list = $("#list");
    list.replaceChildren();
    for (const b of shown) {
      const li = document.createElement("li");
      const a = document.createElement("a");
      a.href = b.url;
      const name = document.createElement("span");
      name.textContent = b.name || b.url;
      const url = document.createElement("small");
      url.textContent = b.url;
      a.append(name, url);
      const del = document.createElement("button");
      del.textContent = "Delete";
      del.onclick = async () => {
        del.disabled = true;
        try {
          await wisp.sync.remove(b.id);
          all = all.filter((x) => x.id !== b.id);
          render();
        } catch (e) {
          $("#status").textContent = "Delete failed: " + e.message;
          del.disabled = false;
        }
      };
      li.append(a, del);
      list.append(li);
    }
    if (!all.length) $("#status").textContent = "No bookmarks yet. Press Ctrl+D or the star on any page.";
  };

  const load = async () => {
    try {
      all = (await wisp.sync.list("b_")).sort((a, b) => a.name.localeCompare(b.name));
      const s = await wisp.sync.status();
      $("#status").textContent = s.configured
        ? `${all.length} bookmark${all.length === 1 ? "" : "s"} · synced with ${new URL(s.service).host}`
        : "";
      render();
    } catch (e) {
      $("#status").textContent = "Could not load bookmarks: " + e.message;
    }
  };

  // Import: the bookmarks.html every browser exports, or Chrome's Bookmarks JSON file.
  const readBookmarks = (text) => {
    if (/^\s*{/.test(text)) {
      const out = [];
      const walk = (node) => {
        if (!node || typeof node !== "object") return;
        if (node.type === "url" && node.url) out.push({ title: node.name || "", url: node.url });
        if (node.uri && /^https?:/.test(node.uri)) out.push({ title: node.title || "", url: node.uri });
        for (const v of Object.values(node)) if (typeof v === "object") walk(v);
      };
      walk(JSON.parse(text));
      return out;
    }
    const doc = new DOMParser().parseFromString(text, "text/html");
    return [...doc.querySelectorAll("a[href]")].map((a) => ({ title: a.textContent.trim(), url: a.getAttribute("href") }));
  };
  $("#import").onclick = () => $("#file").click();
  $("#file").onchange = async () => {
    const file = $("#file").files[0];
    $("#file").value = "";
    if (!file) return;
    try {
      const have = new Set(all.map((b) => b.url));
      const fresh = readBookmarks(await file.text()).filter((b) => {
        if (!/^https?:\/\//.test(b.url) || have.has(b.url)) return false;
        have.add(b.url);
        return true;
      });
      if (!fresh.length) return ($("#status").textContent = "Nothing new to import from " + file.name);
      if (!confirm(`Import ${fresh.length} bookmark${fresh.length === 1 ? "" : "s"} from ${file.name}?`)) return;
      let done = 0;
      for (const b of fresh) {
        await wisp.sync.add({ title: "b_" + (b.title || new URL(b.url).hostname), url: b.url });
        if (++done % 50 === 0) $("#status").textContent = `Importing… ${done} of ${fresh.length}`;
      }
      await load();
      $("#status").textContent = `Imported ${done} bookmarks. ${all.length} in total.`;
    } catch (e) {
      $("#status").textContent = "Import failed: " + e.message;
    }
  };

  $("#q").addEventListener("input", render);
  load();
})();
