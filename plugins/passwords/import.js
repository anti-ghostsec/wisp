// Reads password exports from other browsers and password managers.
globalThis.wispImport = (() => {
  // RFC 4180 CSV: quoted fields, "" escapes, newlines inside quotes.
  const parseCSV = (text) => {
    const rows = [];
    let row = [], field = "", quoted = false;
    text = text.replace(/^﻿/, "");
    for (let i = 0; i < text.length; i++) {
      const c = text[i];
      if (quoted) {
        if (c === '"' && text[i + 1] === '"') { field += '"'; i++; }
        else if (c === '"') quoted = false;
        else field += c;
      } else if (c === '"') quoted = true;
      else if (c === ",") { row.push(field); field = ""; }
      else if (c === "\n" || c === "\r") {
        if (c === "\r" && text[i + 1] === "\n") i++;
        row.push(field); field = "";
        if (row.some((f) => f !== "")) rows.push(row);
        row = [];
      } else field += c;
    }
    row.push(field);
    if (row.some((f) => f !== "")) rows.push(row);
    return rows;
  };

  // The first value that is an http(s) URL; some exports list several.
  const firstUrl = (value) => {
    for (const part of String(value || "").split(/[\s,]+/)) {
      try {
        const u = new URL(/^[a-z]+:\/\//i.test(part) ? part : "https://" + part);
        if (/^https?:$/.test(u.protocol) && u.hostname.includes(".")) return u.origin;
      } catch {}
    }
    return "";
  };

  const COLUMNS = {
    url: ["url", "login_uri", "uri", "website", "web site", "urls", "login url"],
    username: ["username", "login_username", "login", "user", "user name", "login name"],
    email: ["email", "e-mail", "itememail"],
    password: ["password", "login_password"],
    type: ["type"],
  };

  const fromCSV = (text) => {
    const [head, ...rows] = parseCSV(text);
    if (!head) return [];
    const names = head.map((h) => h.trim().toLowerCase());
    const col = (key) => names.findIndex((n) => COLUMNS[key].includes(n));
    const c = Object.fromEntries(Object.keys(COLUMNS).map((k) => [k, col(k)]));
    if (c.password < 0 || c.url < 0) throw new Error("this CSV has no url and password columns");
    return rows
      .filter((r) => c.type < 0 || !r[c.type] || /login|^1$/i.test(r[c.type]))
      .map((r) => ({
        url: firstUrl(r[c.url]),
        username: (c.username >= 0 && r[c.username]) || (c.email >= 0 && r[c.email]) || "",
        password: r[c.password] || "",
      }));
  };

  const fromJSON = (text) => {
    const data = JSON.parse(text);
    const out = [];
    // Proton Pass: { vaults: { id: { items: [{ data: { type, content } }] } } }
    for (const vault of Object.values(data.vaults || {})) {
      for (const item of vault.items || []) {
        const d = item.data || {};
        if (d.type !== "login") continue;
        const c = d.content || {};
        out.push({ url: firstUrl((c.urls || []).join(" ")), username: c.itemUsername || c.username || c.itemEmail || "", password: c.password || "" });
      }
    }
    // Bitwarden: { items: [{ type: 1, login: { username, password, uris } }] }
    for (const item of data.items || []) {
      if (!item.login) continue;
      out.push({ url: firstUrl((item.login.uris || []).map((u) => u.uri).join(" ")), username: item.login.username || "", password: item.login.password || "" });
    }
    // Firefox logins.json-style: { logins: [{ hostname, username, password }] } with plain values.
    for (const l of data.logins || []) {
      if (l.encryptedPassword) throw new Error("this file is encrypted; export the passwords as CSV instead");
      out.push({ url: firstUrl(l.hostname || l.origin || l.url), username: l.username || "", password: l.password || "" });
    }
    return out;
  };

  const parseLogins = (text, filename = "") => {
    const json = /\.json$/i.test(filename) || /^\s*[{[]/.test(text);
    if (/^-----BEGIN PGP/.test(text)) throw new Error("this export is PGP-encrypted; export it unencrypted instead");
    return (json ? fromJSON(text) : fromCSV(text)).filter((l) => l.url && l.password);
  };

  return { parseCSV, parseLogins };
})();
