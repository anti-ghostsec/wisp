// Fills saved logins and offers to save new ones. Runs in the plugin's
// isolated world: the page can't see this code, the credentials or `wisp`.
(() => {
  // Local servers (development, routers, NAS boxes) count too, by address
  // and port, since each port is usually a different app; other plain-HTTP
  // sites don't, as a password typed there travels unencrypted.
  const isLocal = (h) => h === "localhost" || /\.(localhost|test)$/.test(h) || /^\d+\.\d+\.\d+\.\d+$/.test(h) || h.startsWith("[");
  if (location.protocol !== "https:" && !isLocal(location.hostname)) return;
  const hostOf = (u) => (isLocal(u.hostname) ? u.host : u.hostname.replace(/^www\./, ""));
  const host = hostOf(location);
  // Logins are shared across a site's subdomains (accounts.example.com and
  // example.com). A rough registrable domain is enough for that.
  const siteOf = (h) =>
    isLocal(h.replace(/:\d+$/, "")) ? h : h.split(".").slice(/\.(co|com|org|net|gov|ac|edu|ne|or)\.[a-z]{2}$/.test(h) ? -3 : -2).join(".");
  const site = siteOf(host);

  // ---- finding fields ----
  const visible = (el) => el.isConnected && !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length);
  const isPassword = (el) => el instanceof HTMLInputElement && el.type === "password";
  const passwordFields = () => [...document.querySelectorAll('input[type="password"]')].filter(visible);
  const hints = (el) => [el.name, el.id, el.placeholder, el.getAttribute("aria-label"), el.autocomplete].join(" ");
  const USER_HINT = /user|login|email|e-mail|account|identifier|name/i;
  const isUsernameField = (el) =>
    el instanceof HTMLInputElement &&
    !/search|query|captcha|code|otp/i.test(hints(el)) &&
    (/username|email/.test(el.autocomplete) ||
      el.type === "email" ||
      (["text", "tel", ""].includes(el.type) && USER_HINT.test(hints(el))));
  const isTextInput = (el) => ["text", "email", "tel", ""].includes(el.type) && !/search|query|captcha|code|otp/i.test(hints(el));

  const formOf = (el) => el.form || el.closest("form, [role=form], [role=dialog], dialog");
  const before = (a, b) => !!(b.compareDocumentPosition(a) & Node.DOCUMENT_POSITION_PRECEDING);

  // The username input that belongs to a password field: in the same form,
  // one marked as such or else the last text input before it. Without a
  // form, only a nearby field that looks like a username counts, so a
  // search box elsewhere on the page is never taken for one.
  const usernameFieldFor = (pw) => {
    const scope = formOf(pw);
    if (scope) {
      const inputs = [...scope.querySelectorAll("input")].filter((i) => i !== pw && visible(i));
      const marked = inputs.find((i) => /username|email/.test(i.autocomplete));
      if (marked) return marked;
      const earlier = inputs.filter((i) => isTextInput(i) && before(i, pw));
      return earlier.at(-1) || null;
    }
    let el = pw.parentElement;
    for (let depth = 0; el && el !== document.body && depth < 5; depth++, el = el.parentElement) {
      const found = [...el.querySelectorAll("input")].filter((i) => visible(i) && isUsernameField(i) && before(i, pw));
      if (found.length) return found.at(-1);
    }
    return null;
  };

  // Sites often keep the username of a password-only step in a hidden field.
  const hiddenUsername = (pw) => {
    const scope = formOf(pw) || document;
    const el = scope.querySelector('input[autocomplete~="username"], input[name="username"][type="hidden"], input[type="email"][hidden]');
    return el && !visible(el) ? el.value.trim() : "";
  };

  // A signup or change-password form: a field marked new-password, or more
  // than one password field together.
  const isNewPassword = (pw) => {
    if (pw.autocomplete === "new-password") return true;
    const scope = formOf(pw);
    return !!scope && [...scope.querySelectorAll('input[type="password"]')].filter(visible).length > 1;
  };

  const setValue = (el, value) => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
    el.focus();
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
  };

  // ---- saved logins ----
  let saved = null;
  const loadSaved = async (fresh) => {
    if (saved && !fresh) return saved;
    const entries = await wisp.sync.list("p_");
    saved = entries
      .map((e) => {
        try {
          const d = JSON.parse(e.description || "{}");
          const h = hostOf(new URL(e.url));
          return { id: e.id, host: h, username: d.username || "", password: d.password || "" };
        } catch {
          return null;
        }
      })
      .filter((e) => e && e.password && siteOf(e.host) === site);
    lastUsed = (await wisp.storage.get("used")) || {};
    // Forget logins that no longer exist.
    const ids = new Set(entries.map((e) => String(e.id)));
    const stale = Object.keys(lastUsed).filter((id) => !ids.has(id));
    if (stale.length) {
      stale.forEach((id) => delete lastUsed[id]);
      wisp.storage.set("used", lastUsed);
    }
    sortSaved();
    return saved;
  };
  // When each login (by id) was last filled or used to log in, kept on this
  // computer only. The most recently used come first, then the newest saved;
  // logins saved on this exact host before the rest of the site's.
  let lastUsed = {};
  const sortSaved = () =>
    saved.sort((a, b) => (lastUsed[b.id] || 0) - (lastUsed[a.id] || 0) || (b.host === host) - (a.host === host) || b.id - a.id);
  // Sent at once, without waiting on anything: logging in usually leaves
  // the page within milliseconds.
  const markUsed = (cred) => {
    if (!cred) return;
    lastUsed = { ...lastUsed, [cred.id]: Date.now() };
    wisp.storage.set("used", lastUsed);
    if (saved) sortSaved();
  };


  // Usernames typed on a "username first, password on the next step" page.
  let typedUsername = "";
  const rememberUsername = (value) => {
    if (!value || value === typedUsername) return;
    typedUsername = value;
    wisp.session.set("user:" + site, { value, at: Date.now() });
  };
  const recentUsername = async () => {
    if (typedUsername) return typedUsername;
    const u = await wisp.session.get("user:" + site);
    return u && Date.now() - u.at < 10 * 60 * 1000 ? u.value : "";
  };

  const strongPassword = () => {
    const sets = ["ABCDEFGHJKLMNPQRSTUVWXYZ", "abcdefghijkmnopqrstuvwxyz", "23456789", "-_.!@#$%*?"];
    const all = sets.join("");
    const pick = (chars) => {
      // Rejection sampling keeps every character equally likely.
      const limit = 256 - (256 % chars.length);
      for (;;) {
        const [b] = crypto.getRandomValues(new Uint8Array(1));
        if (b < limit) return chars[b % chars.length];
      }
    };
    for (;;) {
      const pw = Array.from({ length: 20 }, () => pick(all)).join("");
      if (sets.every((s) => [...pw].some((c) => s.includes(c)))) return pw;
    }
  };

  // ---- small shadow-DOM UI pieces ----
  const KEY_SVG = `<svg viewBox="0 0 16 16" width="14" height="14"><path fill="currentColor" d="M5 4a4 4 0 1 0 3.87 5H10v2h2V9h1v2h2V7H8.87A4 4 0 0 0 5 4Zm0 2a2 2 0 1 1 0 4 2 2 0 0 1 0-4Z"/></svg>`;
  const panel = (css, build) => {
    const hostEl = document.createElement("div");
    const root = hostEl.attachShadow({ mode: "closed" });
    root.innerHTML = `<style>
      .box { position: fixed; z-index: 2147483647; font: 13px system-ui, sans-serif; color: #eee;
             background: #222; border-radius: 8px; box-shadow: 0 4px 18px #0006; padding: 6px; ${css} }
      button { font: inherit; color: inherit; background: #333; border: 0; border-radius: 5px;
               padding: 6px 10px; margin: 2px; cursor: pointer; }
      button:hover { background: #444; } button.main { background: #3a6ee8; }
      .item { display: block; width: calc(100% - 4px); text-align: left; background: none; white-space: nowrap; }
      .item small { opacity: .6; margin-left: 6px; }
      hr { border: 0; border-top: 1px solid #fff2; margin: 4px 2px; }
      p { margin: 4px 6px 8px; }
    </style>`;
    const box = Object.assign(document.createElement("div"), { className: "box" });
    root.append(box);
    build(box, () => hostEl.remove());
    document.documentElement.appendChild(hostEl);
    return hostEl;
  };

  const toast = (text) => {
    const el = panel("bottom: 16px; right: 16px; padding: 10px 14px;", (box) => (box.textContent = text));
    setTimeout(() => el.remove(), 3500);
  };

  // ---- the key button and its menu ----
  let menu = null;
  const closeMenu = () => { menu?.remove(); menu = null; };

  const openMenu = async (field) => {
    closeMenu();
    const creds = await loadSaved();
    const pw = isPassword(field) ? field : null;
    const items = [];
    const known = pw ? (usernameFieldFor(pw)?.value || hiddenUsername(pw) || (await recentUsername())) : "";
    const ordered = known ? [...creds.filter((c) => c.username === known), ...creds.filter((c) => c.username !== known)] : creds;
    for (const c of ordered) {
      items.push({ label: c.username || "(no username)", note: c.host !== host ? c.host : "", run: () => fillFrom(field, c) });
    }
    if (pw) {
      items.push({ sep: true });
      items.push({ label: "Generate strong password", run: () => generateInto(field) });
    }
    items.push({ label: "Manage passwords", run: () => wisp.tabs.open("wisp-plugin://passwords/list.html") });
    const r = field.getBoundingClientRect();
    menu = panel(`left:${Math.max(4, r.left)}px; top:${r.bottom + 4}px; min-width:${Math.max(200, Math.min(r.width, 360))}px;`, (box) => {
      for (const it of items) {
        if (it.sep) {
          if (box.childElementCount) box.append(document.createElement("hr"));
          continue;
        }
        const b = document.createElement("button");
        b.className = "item";
        b.textContent = it.label;
        if (it.note) b.append(Object.assign(document.createElement("small"), { textContent: it.note }));
        // mousedown keeps focus in the field, so the menu doesn't close first.
        b.onmousedown = (ev) => {
          ev.preventDefault();
          closeMenu();
          it.run();
        };
        box.append(b);
      }
    });
  };

  // The key sits inside the right end of the focused field.
  let keyButton = null, keyEl = null, keyFor = null;
  const placeKey = () => {
    if (!keyButton || !keyFor) return;
    const r = keyFor.getBoundingClientRect();
    const k = keyEl;
    if (!r.width || r.bottom < 0 || r.top > innerHeight) return (keyButton.style.display = "none");
    keyButton.style.display = "";
    k.style.left = `${r.right - 26}px`;
    k.style.top = `${r.top + (r.height - 22) / 2}px`;
  };
  const hideKey = () => { keyButton?.remove(); keyButton = keyEl = keyFor = null; };
  const showKey = (field) => {
    hideKey();
    keyFor = field;
    keyButton = document.createElement("div");
    const root = keyButton.attachShadow({ mode: "closed" });
    root.innerHTML = `<style>.key{all:initial;position:fixed;z-index:2147483647;display:flex;align-items:center;justify-content:center;
      width:22px;height:22px;border-radius:5px;cursor:pointer;color:#555;background:#fffd;box-shadow:0 0 0 1px #0002}
      .key:hover{color:#000;background:#fff}</style><div class="key" title="Passwords">${KEY_SVG}</div>`;
    const k = (keyEl = root.querySelector(".key"));
    k.onmousedown = (ev) => {
      ev.preventDefault();
      menu ? closeMenu() : openMenu(field);
    };
    document.documentElement.appendChild(keyButton);
    placeKey();
  };
  addEventListener("scroll", () => { placeKey(); closeMenu(); }, { capture: true, passive: true });
  addEventListener("resize", placeKey, { passive: true });

  // Fields the key button shows on: every password field, and username
  // fields on sites with saved logins.
  document.addEventListener("focusin", async (ev) => {
    const el = ev.target;
    if (!(el instanceof HTMLInputElement)) return;
    if (isPassword(el)) {
      showKey(el);
      const creds = await loadSaved();
      // Offer the choice straight away when it isn't obvious.
      if (!el.value && (isNewPassword(el) ? !el.dataset.wispOffered : creds.length > 1)) {
        el.dataset.wispOffered = "1";
        openMenu(el);
      }
    } else if (isUsernameField(el) && (await loadSaved()).length) {
      showKey(el);
      if (!el.value && !el.dataset.wispOffered) {
        el.dataset.wispOffered = "1";
        openMenu(el);
      }
    }
  }, true);
  document.addEventListener("focusout", (ev) => {
    if (ev.target === keyFor) setTimeout(() => { if (document.activeElement !== keyFor) { hideKey(); closeMenu(); } }, 150);
  }, true);

  // ---- filling ----
  const fillFrom = (field, cred) => {
    markUsed(cred);
    if (isPassword(field)) {
      const user = usernameFieldFor(field);
      if (user && cred.username && user.value !== cred.username) setValue(user, cred.username);
      setValue(field, cred.password);
      return;
    }
    if (cred.username) setValue(field, cred.username);
    // A password field in the same form gets the password too.
    const scope = formOf(field) || document;
    const pw = [...scope.querySelectorAll('input[type="password"]')].find(visible);
    if (pw) setValue(pw, cred.password);
    field.focus();
  };

  // Fill this field and the matching confirm field(s) with a new password.
  const generateInto = (field) => {
    const password = strongPassword();
    const scope = formOf(field);
    const targets = isPassword(field) && scope
      ? [...scope.querySelectorAll('input[type="password"]')].filter((p) => visible(p) && (p === field || p.autocomplete === "new-password" || !p.value || before(field, p)))
      : [field];
    for (const t of targets) setValue(t, password);
    field.focus();
    draft = { pw: field, password, generated: true };
    toast("Strong password filled in. Wisp will offer to save it when you submit.");
  };

  let filledUser = false, filledPassword = false;
  const tryFill = async () => {
    const creds = await loadSaved();
    if (!creds.length) return;
    const fields = passwordFields();
    // Username-only step of a two-step login.
    if (!fields.length) {
      if (filledUser) return;
      const user = [...document.querySelectorAll("input")].find((i) => visible(i) && (/username|email/.test(i.autocomplete) || i.type === "email"));
      if (user && !user.value && creds.length === 1 && creds[0].username) {
        setValue(user, creds[0].username);
        user.blur();
        filledUser = true;
      }
      return;
    }
    if (filledPassword) return;
    const remembered = await recentUsername();
    for (const pw of fields) {
      if (pw.dataset.wispSeen || pw.value || isNewPassword(pw)) continue;
      pw.dataset.wispSeen = "1";
      const user = usernameFieldFor(pw);
      const known = user?.value.trim() || hiddenUsername(pw) || (user ? "" : remembered);
      let pick = null;
      if (known) {
        const same = creds.filter((c) => c.username === known);
        if (same.length === 1) pick = same[0];
      } else if (user && creds.length === 1) {
        // An ordinary login form with one saved login for the site.
        pick = creds[0];
      }
      // A password field on its own with no username we know of is usually
      // "confirm your password" while logged in: only the key button there.
      if (pick) {
        fillFrom(pw, pick);
        pw.blur();
        filledPassword = true;
        return;
      }
    }
  };

  // ---- saving ----
  // The newest values typed into a password field.
  let draft = null;
  document.addEventListener("input", (ev) => {
    const el = ev.target;
    if (!(el instanceof HTMLInputElement)) return;
    if (isPassword(el)) {
      if (!draft?.generated || el.value !== draft.password) draft = { pw: el, password: el.value };
    } else if (isUsernameField(el)) {
      typedUsername = el.value.trim();
    }
  }, true);
  document.addEventListener("change", (ev) => {
    if (ev.target instanceof HTMLInputElement && isUsernameField(ev.target)) rememberUsername(ev.target.value.trim());
  }, true);

  let captured = null;
  // Record the login the moment it's submitted: a login usually navigates
  // away within milliseconds, so nothing here may wait. Whether it's worth
  // offering is decided later, on the page that follows.
  const capture = () => {
    if (!draft || !draft.password) return;
    const { pw, password } = draft;
    if (captured === password) return;
    captured = password;
    if (typedUsername) rememberUsername(typedUsername);
    const scope = formOf(pw);
    const pending = {
      username: usernameFieldFor(pw)?.value.trim() || hiddenUsername(pw) || typedUsername,
      password,
      // All password values of a change-password form; the current one picks the login.
      values: scope ? [...scope.querySelectorAll('input[type="password"]')].map((p) => p.value) : [password],
      isNew: isNewPassword(pw),
      host,
      origin: location.origin,
      at: Date.now(),
    };
    wisp.session.set("pending:" + site, pending);
    markUsed(saved?.find((c) => c.password === password && (!pending.username || !c.username || c.username === pending.username)));
    watchForSuccess(pw);
  };

  // Offer to save once the login seems to have worked: the page navigated
  // (checked on the next page), the URL changed, or the form went away. A
  // failed login usually leaves the password field in place.
  const watchForSuccess = (pw) => {
    const startUrl = location.href;
    let n = 0;
    const timer = setInterval(() => {
      if (++n > 40) return clearInterval(timer);
      if (location.href !== startUrl || !visible(pw)) {
        clearInterval(timer);
        checkPending();
      }
    }, 500);
  };

  let offered = false;
  const checkPending = async () => {
    if (offered) return;
    const key = "pending:" + site;
    const p = await wisp.session.get(key);
    if (!p) return;
    const drop = () => wisp.session.remove(key);
    if (Date.now() - p.at > 2 * 60 * 1000) return drop();
    // An empty login form here means the login failed, or you've logged out
    // since: don't offer a password from earlier.
    if (passwordFields().some((f) => !f.value)) return drop();
    const never = (await wisp.storage.get("never")) || [];
    if (never.includes(site) || never.includes(host)) return drop();
    const creds = await loadSaved(true);
    // Already saved, under any username.
    if (creds.some((c) => c.password === p.password)) return drop();
    let username = p.username;
    let update = null;
    if (p.isNew) {
      // Change-password form: the current password picks the login to update.
      update = creds.find((c) => p.values.includes(c.password)) || null;
      if (update) username = update.username;
    }
    if (!username) username = await recentUsername();
    // A lone password field while saved logins exist was a "confirm your
    // password" prompt, not a new login.
    if (!username && !update && creds.length && !p.isNew) return drop();
    if (!update) update = creds.find((c) => c.username === username) || null;
    offered = true;
    offerSave({ ...p, username, update: update?.id ?? null });
  };

  const offerSave = (cred) => {
    panel("top: 14px; right: 14px; max-width: 340px;", (box, close) => {
      const p = document.createElement("p");
      const who = cred.username ? " for " + cred.username : "";
      p.textContent = cred.update ? `Update the saved password${who}?` : `Save password${who} on ${cred.host}?`;
      const save = Object.assign(document.createElement("button"), { textContent: cred.update ? "Update" : "Save", className: "main" });
      const later = Object.assign(document.createElement("button"), { textContent: "Not now" });
      const never = Object.assign(document.createElement("button"), { textContent: "Never for this site" });
      save.onclick = async () => {
        close();
        await wisp.session.remove("pending:" + site);
        try {
          const list = await loadSaved(true);
          // The login being updated, or one saved without a username that
          // we now know the username of.
          const existing =
            list.find((e) => e.id === cred.update) ||
            list.find((e) => e.username === cred.username) ||
            (cred.username && list.find((e) => !e.username && e.password === cred.password));
          const description = JSON.stringify({ username: cred.username, password: cred.password });
          if (existing) await wisp.sync.update(existing.id, { description });
          else await wisp.sync.add({ title: "p_" + cred.host, url: cred.origin, description });
          saved = null;
        } catch (e) {
          console.error("wisp passwords:", e);
        }
      };
      later.onclick = () => { close(); wisp.session.remove("pending:" + site); };
      never.onclick = async () => {
        close();
        await wisp.session.remove("pending:" + site);
        const blocked = (await wisp.storage.get("never")) || [];
        await wisp.storage.set("never", [...new Set([...blocked, site])]);
      };
      box.append(p, save, later, never);
    });
  };

  document.addEventListener("submit", () => capture(), true);
  // Many sites log in with a script instead of a real form submit, so a
  // click near the password field (its login button) counts too.
  const nearField = (target, pw) => {
    let box = formOf(pw) || pw.parentElement;
    for (let depth = 0; box && box !== document.body && depth < 6; depth++, box = box.parentElement) {
      if (box.contains(target)) return true;
    }
    return false;
  };
  document.addEventListener("click", (ev) => {
    if (!draft || !draft.pw.isConnected || ev.target.closest?.("input:not([type=submit]):not([type=button]), textarea, select")) return;
    if (nearField(ev.target, draft.pw)) capture();
  }, true);
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter" && ev.target instanceof HTMLInputElement) capture();
  }, true);

  // ---- right-click menu ----
  let rightClicked = null;
  document.addEventListener("contextmenu", (ev) => { rightClicked = ev.target; }, true);
  wisp.on("context-menu", ({ id }) => {
    const field = rightClicked instanceof HTMLInputElement ? rightClicked : null;
    if (!field) return;
    if (id === "generate") generateInto(field);
    else if (id === "fill") { field.focus(); openMenu(field); }
  });

  checkPending();
  tryFill().then(() => {
    if (!saved?.length) return;
    // Single-page apps render login forms whenever they like, and two-step
    // logins show the password field after the username step.
    let queued = false;
    const started = Date.now();
    const observer = new MutationObserver(() => {
      if (filledPassword || Date.now() - started > 5 * 60 * 1000) return observer.disconnect();
      if (queued) return;
      queued = true;
      setTimeout(() => { queued = false; tryFill(); }, 400);
    });
    observer.observe(document.documentElement, { childList: true, subtree: true });
  });
})();
