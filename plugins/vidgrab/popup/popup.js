// The toolbar popup. shared.js runs first (plugin.json).
/* global PRESETS, getOptions */

const $ = (id) => document.getElementById(id);
let tab = null;

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") node.className = v;
    else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
    else node.setAttribute(k, v);
  }
  for (const c of children) if (c != null) node.append(c);
  return node;
}

function formatSize(bytes) {
  if (!bytes) return "";
  const units = ["B", "KB", "MB", "GB"];
  let i = 0;
  while (bytes >= 1024 && i < units.length - 1) { bytes /= 1024; i++; }
  return `${bytes.toFixed(i ? 1 : 0)} ${units[i]}`;
}

function presetLabel(id) {
  return (PRESETS.find((p) => p[0] === id) || [id, id])[1];
}

function renderHost(status) {
  const badge = $("host-badge");
  const ready = status && !status.error && status.ytdlp;
  $("setup").hidden = !!ready || !status;
  $("install-output").textContent = status?.updateOutput || "";
  $("main").hidden = !ready;
  if (!status) {
    badge.textContent = "…";
    badge.className = "pill";
  } else if (status.installing) {
    badge.textContent = "setting up…";
    badge.className = "pill";
    $("setup-error").textContent = "Downloading yt-dlp (a few MB, once). It updates itself after that.";
    $("install").hidden = true;
  } else if (ready) {
    badge.textContent = `yt-dlp ${status.ytdlp}`;
    badge.className = "pill ok";
    badge.title = status.ffmpeg ? "" : "ffmpeg not found — merging and MP3 conversion won't work";
    if (!status.ffmpeg) badge.textContent += " · no ffmpeg";
  } else {
    badge.textContent = "not ready";
    badge.className = "pill err";
    $("install").hidden = false;
    $("setup-error").textContent = status.error
      || (status.hostVersion ? "Helper is installed, but yt-dlp was not found." : "");
  }
}

function renderMedia(media) {
  const list = $("media");
  list.replaceChildren();
  $("media-section").hidden = media.length === 0;
  for (const m of media) {
    const name = decodeURIComponent(new URL(m.url).pathname.split("/").pop() || m.url);
    const isFile = !/HLS|DASH/.test(m.kind);
    list.append(el("li", {},
      el("div", { class: "media-item" },
        el("span", { class: "kind" }, /HLS|DASH/.test(m.kind) ? m.kind : m.kind.split("/").pop().toUpperCase()),
        el("span", { class: "url", title: m.url }, name),
        m.size ? el("span", { class: "muted" }, formatSize(m.size)) : null,
        el("button", {
          class: "small", title: "Download with yt-dlp",
          onclick: () => send({ type: "download", url: m.url, referer: m.referer || tab.url, preset: $("preset").value, title: tab.title, nameFromPage: true }),
        }, "Get"),
        isFile ? el("button", {
          class: "small", title: "Save directly with the browser",
          onclick: () => send({ type: "browser-download", url: m.url }),
        }, "Save") : null,
      )));
  }
}

function renderJobs(jobs) {
  const list = $("jobs");
  list.replaceChildren();
  if (!jobs.length) {
    list.append(el("li", { class: "muted empty" }, "Nothing yet."));
    return;
  }
  for (const j of jobs) {
    const active = j.state === "running" || j.state === "queued";
    const pct = j.state === "done" ? 100 : (j.percent || 0);
    let meta = "";
    if (j.state === "done") meta = "Finished";
    else if (j.state === "cancelled") meta = "Cancelled";
    else if (j.state === "error") meta = "Failed";
    else if (j.stage) meta = j.stage;
    else meta = [`${pct.toFixed(1)}%`, j.speed, j.eta && `ETA ${j.eta}`].filter((s) => s && !/Unknown|N\/A/.test(s)).join(" · ");

    list.append(el("li", {},
      el("div", { class: "job-head" },
        el("span", { class: "title", title: j.url }, j.title || j.url),
        active
          ? el("button", { class: "small", onclick: () => send({ type: "cancel", id: j.id }) }, "Cancel")
          : el("button", { class: "icon", title: "Remove", onclick: () => send({ type: "remove-job", id: j.id }) }, "×"),
      ),
      el("div", { class: `bar ${j.state}` }, el("div", { style: `width:${pct}%` })),
      el("div", { class: "meta" },
        el("span", {}, meta),
        j.state === "done"
          ? el("button", { class: "link", onclick: () => send({ type: "open", path: j.files?.[0] || j.dir, reveal: true }) }, "Show in folder")
          : el("span", {}, presetLabel(j.preset)),
      ),
      j.state === "error" && j.error ? el("div", { class: "error-text" }, j.error) : null,
    ));
  }
}

function send(msg) {
  return wisp.runtime.sendMessage(msg);
}

async function refresh() {
  const state = await send({ type: "get-state", tabId: tab?.id });
  renderHost(state.hostStatus);
  renderMedia(state.media);
  renderJobs(state.jobs);
}

async function init() {
  tab = await wisp.tabs.current();
  const opts = await getOptions();

  for (const [id, label] of PRESETS) $("preset").append(el("option", { value: id }, label));
  $("preset").value = opts.defaultPreset;

  $("page-title").textContent = tab?.title || "";
  $("page-url").textContent = tab?.url || "";
  const downloadable = /^https?:/.test(tab?.url || "");
  $("download").disabled = !downloadable;

  $("download").addEventListener("click", () => {
    send({ type: "download", url: tab.url, title: tab.title, preset: $("preset").value });
  });
  // Wait for the tab to open: closing the popup first would cancel the request.
  $("settings").addEventListener("click", async () => { await wisp.tabs.open("wisp-plugin://vidgrab/options/options.html"); window.close(); });
  $("install").addEventListener("click", () => send({ type: "install-ytdlp" }));
  $("retry").addEventListener("click", async () => { await send({ type: "refresh-status" }); setTimeout(refresh, 800); });
  $("clear").addEventListener("click", () => send({ type: "clear-finished" }));
  $("open-folder").addEventListener("click", () => send({ type: "open", path: opts.outputDir || "" }));

  wisp.on("broadcast", (msg) => {
    if (msg.type === "state-changed") refresh();
  });
  refresh();
}

init();
