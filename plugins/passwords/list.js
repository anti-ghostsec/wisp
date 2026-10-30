// The password manager page (runs in the plugin's isolated world).
(() => {
  const $ = (s) => document.querySelector(s);
  const DOTS = "••••••••";
  let all = [];
  let editing = null;

  const hostOf = (url) => { try { return new URL(url).hostname; } catch { return ""; } };
  const toEntry = (e) => {
    let d = {};
    try { d = JSON.parse(e.description || "{}"); } catch {}
    return { id: e.id, url: e.url, site: hostOf(e.url), username: d.username || "", password: d.password || "" };
  };

  const strongPassword = () => {
    const chars = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789-_.!@#$%*?";
    const limit = 256 - (256 % chars.length);
    let pw = "";
    while (pw.length < 20) {
      for (const b of crypto.getRandomValues(new Uint8Array(32))) if (b < limit && pw.length < 20) pw += chars[b % chars.length];
    }
    return pw;
  };

  const button = (text, onclick, cls) => Object.assign(document.createElement("button"), { textContent: text, onclick, className: cls || "" });
  const cell = (...children) => {
    const td = document.createElement("td");
    td.append(...children);
    return td;
  };
  const input = (value, placeholder, type = "text") => Object.assign(document.createElement("input"), { value, placeholder, type, spellcheck: false });

  const status = (text) => ($("#status").textContent = text);
  const summary = () =>
    status(all.length ? `${all.length} saved login${all.length === 1 ? "" : "s"}` : "No saved passwords yet. Log in to a site and choose Save when asked, or import them.");

  const save = async (entry, fields) => {
    const url = /^https?:\/\//.test(fields.url) ? fields.url : "https://" + fields.url;
    let origin;
    try { origin = new URL(url).origin; } catch { return status("That site address isn't valid."); }
    if (!fields.password) return status("The password can't be empty.");
    const description = JSON.stringify({ username: fields.username.trim(), password: fields.password });
    const title = "p_" + new URL(origin).hostname;
    try {
      if (entry) {
        await wisp.sync.update(entry.id, { title, url: origin, description });
        Object.assign(entry, { url: origin, site: hostOf(origin), username: fields.username.trim(), password: fields.password });
      } else {
        const id = await wisp.sync.add({ title, url: origin, description });
        all.push({ id, url: origin, site: hostOf(origin), username: fields.username.trim(), password: fields.password });
        all.sort((a, b) => a.site.localeCompare(b.site));
      }
      editing = null;
      render();
    } catch (e) {
      status("Saving failed: " + e.message);
    }
  };

  const editRow = (entry) => {
    const tr = document.createElement("tr");
    const site = input(entry ? entry.url : "", "https://example.com");
    const user = input(entry ? entry.username : "", "Username or email");
    const pw = input(entry ? entry.password : "", "Password");
    const gen = button("Generate", () => { pw.value = strongPassword(); });
    const ok = button(entry ? "Save" : "Add", () => save(entry, { url: site.value.trim(), username: user.value, password: pw.value }), "main");
    const cancel = button("Cancel", () => { editing = null; render(); });
    for (const el of [site, user, pw]) {
      el.addEventListener("keydown", (ev) => {
        if (ev.key === "Enter") ok.click();
        if (ev.key === "Escape") cancel.click();
      });
    }
    const actions = cell(gen, ok, cancel);
    actions.className = "actions";
    tr.append(cell(site), cell(user), cell(pw), actions);
    setTimeout(() => (entry ? pw : site).focus());
    return tr;
  };

  const viewRow = (e) => {
    const tr = document.createElement("tr");
    const code = Object.assign(document.createElement("code"), { textContent: DOTS });
    const show = button("Show", () => {
      const hidden = code.textContent === DOTS;
      code.textContent = hidden ? e.password : DOTS;
      show.textContent = hidden ? "Hide" : "Show";
    });
    const copy = button("Copy", () => wisp.clipboard.write(e.password, { secret: true }).then(() => status("Password copied; cleared from the clipboard in 45 seconds"), (err) => status("Copy failed: " + err.message)));
    const actions = cell(
      button("Edit", () => { editing = e.id; render(); }),
      button("Delete", async () => {
        if (!confirm(`Delete the saved password for ${e.username || e.site}?`)) return;
        try {
          await wisp.sync.remove(e.id);
          all = all.filter((x) => x.id !== e.id);
          render();
        } catch (err) {
          status("Delete failed: " + err.message);
        }
      }),
    );
    actions.className = "actions";
    const site = Object.assign(document.createElement("a"), { href: e.url, textContent: e.site, target: "_blank" });
    const user = cell(e.username);
    user.title = "Double-click to copy";
    user.ondblclick = () => wisp.clipboard.write(e.username).then(() => status("Username copied"), (err) => status("Copy failed: " + err.message));
    tr.append(cell(site), user, cell(code, show, copy), actions);
    return tr;
  };

  const render = () => {
    const q = $("#q").value.trim().toLowerCase();
    const rows = $("#rows");
    rows.replaceChildren();
    if (editing === "new") rows.append(editRow(null));
    for (const e of all.filter((e) => !q || (e.site + " " + e.username).toLowerCase().includes(q))) {
      rows.append(editing === e.id ? editRow(e) : viewRow(e));
    }
    summary();
  };

  // ---- import ----
  let incoming = null;
  const key = (l) => hostOf(l.url).replace(/^www\./, "") + "\n" + l.username;
  $("#import").onclick = () => {
    $("#import-hint").hidden = false;
    $("#file").click();
  };
  $("#file").onchange = async () => {
    const file = $("#file").files[0];
    $("#file").value = "";
    if (!file) return;
    try {
      const logins = wispImport.parseLogins(await file.text(), file.name);
      const existing = new Map(all.map((e) => [key(e), e]));
      const seen = new Set();
      incoming = { add: [], change: [], same: 0 };
      for (const l of logins) {
        const k = key(l);
        if (seen.has(k)) continue;
        seen.add(k);
        const old = existing.get(k);
        if (!old) incoming.add.push(l);
        else if (old.password !== l.password) incoming.change.push({ old, l });
        else incoming.same++;
      }
      const parts = [`${incoming.add.length} new`];
      if (incoming.change.length) parts.push(`${incoming.change.length} with a different password (will be updated)`);
      if (incoming.same) parts.push(`${incoming.same} already saved`);
      $("#import-summary").textContent = `${file.name}: ${logins.length} login${logins.length === 1 ? "" : "s"} found — ${parts.join(", ")}.`;
      $("#import-go").disabled = !incoming.add.length && !incoming.change.length;
      $("#import-panel").hidden = false;
    } catch (e) {
      status("Couldn't read that file: " + e.message);
    }
  };
  $("#import-cancel").onclick = () => { incoming = null; $("#import-panel").hidden = true; };
  $("#import-go").onclick = async () => {
    if (!incoming) return;
    const { add, change } = incoming;
    incoming = null;
    $("#import-panel").hidden = true;
    let done = 0;
    try {
      for (const l of add) {
        const origin = new URL(l.url).origin;
        await wisp.sync.add({ title: "p_" + hostOf(origin), url: origin, description: JSON.stringify({ username: l.username, password: l.password }) });
        if (++done % 25 === 0) status(`Importing… ${done} of ${add.length + change.length}`);
      }
      for (const { old, l } of change) {
        await wisp.sync.update(old.id, { description: JSON.stringify({ username: old.username, password: l.password }) });
        done++;
      }
      await load();
      status(`Imported ${done} login${done === 1 ? "" : "s"}. ${all.length} saved in total.`);
    } catch (e) {
      await load();
      status(`Import stopped after ${done}: ${e.message}`);
    }
  };

  const load = async () => {
    try {
      all = (await wisp.sync.list("p_")).map(toEntry).sort((a, b) => a.site.localeCompare(b.site));
      render();
    } catch (err) {
      status("Could not load passwords: " + err.message);
    }
  };

  $("#add").onclick = () => { editing = "new"; render(); };
  $("#q").addEventListener("input", render);
  load();
})();
