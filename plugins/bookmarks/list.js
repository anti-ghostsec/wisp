// Renders list.html (runs in the plugin's isolated world, so it has the wisp API).
(() => {
  const $ = (s) => document.querySelector(s);
  let all = [];

  // Shown in their saved order, which is also the bookmarks bar's order.
  // Drag a row by its handle to move it; the first row is the bar's first.
  let dragging = null;
  const clearMarks = () => document.querySelectorAll(".over-top, .over-bottom").forEach((el) => el.classList.remove("over-top", "over-bottom"));

  const move = async (id, target, after) => {
    // Show it at once; the order is saved and synced in the background.
    const item = all.find((b) => b.id === id);
    all = all.filter((b) => b.id !== id);
    all.splice(all.findIndex((b) => b.id === target) + (after ? 1 : 0), 0, item);
    render();
    try {
      await wisp.sync.move(id, target, after);
    } catch (e) {
      $("#status").textContent = "Moving failed: " + e.message;
      load();
    }
  };

  const editRow = (b) => {
    const li = document.createElement("li");
    li.className = "editing";
    const name = Object.assign(document.createElement("input"), { value: b.name, placeholder: "Name" });
    const url = Object.assign(document.createElement("input"), { value: b.url, placeholder: "https://…" });
    const save = Object.assign(document.createElement("button"), { textContent: "Save", className: "main" });
    const cancel = Object.assign(document.createElement("button"), { textContent: "Cancel" });
    save.onclick = async () => {
      const u = url.value.trim();
      if (!/^https?:\/\/./.test(u)) return ($("#status").textContent = "The address must start with https:// or http://");
      try {
        await wisp.sync.update(b.id, { title: "b_" + (name.value.trim() || new URL(u).hostname), url: u });
        Object.assign(b, { name: name.value.trim() || new URL(u).hostname, url: u });
        editing = null;
        render();
      } catch (e) {
        $("#status").textContent = "Saving failed: " + e.message;
      }
    };
    cancel.onclick = () => { editing = null; render(); };
    for (const el of [name, url]) el.onkeydown = (ev) => { if (ev.key === "Enter") save.click(); if (ev.key === "Escape") cancel.click(); };
    li.append(name, url, save, cancel);
    setTimeout(() => name.focus());
    return li;
  };

  let editing = null;
  const render = () => {
    const q = $("#q").value.trim().toLowerCase();
    const shown = all.filter((b) => !q || (b.name + " " + b.url).toLowerCase().includes(q));
    const list = $("#list");
    list.replaceChildren();
    for (const b of shown) {
      if (editing === b.id) {
        list.append(editRow(b));
        continue;
      }
      const li = document.createElement("li");
      // Reordering a filtered list would be confusing, so only unfiltered.
      const handle = Object.assign(document.createElement("span"), { className: "handle", textContent: "⠿", title: "Drag to move" });
      handle.hidden = !!q;
      li.draggable = !q;
      li.ondragstart = (ev) => {
        dragging = b.id;
        ev.dataTransfer.effectAllowed = "move";
        ev.dataTransfer.setData("text/plain", b.url);
        li.classList.add("dragging");
      };
      li.ondragend = () => { dragging = null; li.classList.remove("dragging"); clearMarks(); };
      li.ondragover = (ev) => {
        if (dragging === null || dragging === b.id) return;
        ev.preventDefault();
        const r = li.getBoundingClientRect();
        clearMarks();
        li.classList.add(ev.clientY > r.top + r.height / 2 ? "over-bottom" : "over-top");
      };
      li.ondragleave = () => li.classList.remove("over-top", "over-bottom");
      li.ondrop = (ev) => {
        ev.preventDefault();
        const after = li.classList.contains("over-bottom");
        clearMarks();
        if (dragging !== null && dragging !== b.id) move(dragging, b.id, after);
      };
      const a = document.createElement("a");
      a.href = b.url;
      a.draggable = false;
      const name = document.createElement("span");
      name.textContent = b.name || b.url;
      const url = document.createElement("small");
      url.textContent = b.url;
      a.append(name, url);
      const edit = Object.assign(document.createElement("button"), { textContent: "Edit" });
      edit.onclick = () => { editing = b.id; render(); };
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
      li.append(handle, a, edit, del);
      list.append(li);
    }
    if (!all.length) $("#status").textContent = "No bookmarks yet. Press Ctrl+D or the star on any page.";
  };

  const load = async () => {
    try {
      all = await wisp.sync.list("b_");
      const s = await wisp.sync.status();
      $("#status").textContent = s.configured
        ? `${all.length} bookmark${all.length === 1 ? "" : "s"} · synced with ${new URL(s.service).host} · drag ⠿ to reorder; the top ones come first on the bookmarks bar`
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
