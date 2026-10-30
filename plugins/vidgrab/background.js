// VidGrab's background script: keeps the download list, talks to the helper
// (host/vidgrab_host.py, which runs yt-dlp) and collects video streams that
// pages load. shared.js is loaded first (plugin.json).
/* global getOptions */

let connected = false;
let hostStatus = null; // last "status" reply, or {error} if the host can't start
const jobs = new Map(); // id -> job object
const sniffed = new Map(); // tabId -> Map(url -> media item)

// ---------- native host ----------

// Wisp starts the helper on the first message and reports when it stops.
wisp.on("native", (msg) => onHostMessage(msg));
wisp.on("native-disconnect", ({ error }) => {
  {
    connected = false;
    hostStatus = { error: error || "Native host disconnected" };
    for (const job of jobs.values()) {
      if (job.state === "running" || job.state === "queued") {
        job.state = "error";
        job.error = "Lost connection to native host";
      }
    }
    broadcast();
  }
});

async function postToHost(msg) {
  try {
    await wisp.native.post(msg);
    connected = true;
    return true;
  } catch (e) {
    hostStatus = { error: String(e.message || e) };
    broadcast();
    return false;
  }
}

async function refreshStatus() {
  const opts = await getOptions();
  postToHost({ action: "status", ytdlpPath: opts.ytdlpPath });
}

function onHostMessage(msg) {
  if (msg.type === "status") {
    hostStatus = msg;
  } else if (msg.type === "update") {
    hostStatus = Object.assign({}, hostStatus, { updateOutput: msg.output, updateOk: msg.ok });
    refreshStatus();
  } else if (msg.type === "job") {
    const job = jobs.get(msg.id);
    if (!job) return;
    const prevState = job.state;
    for (const k of ["state", "percent", "speed", "eta", "title", "stage", "error", "files", "dir", "warning"]) {
      if (k === "title" && job.fixedTitle) continue;
      if (msg[k] !== undefined) job[k] = msg[k];
    }
    if (msg.percent !== undefined) job.stage = "";
    if (prevState !== job.state && (job.state === "done" || job.state === "error")) {
      notifyJob(job);
    }
  } else if (msg.type === "error") {
    const job = msg.id && jobs.get(msg.id);
    if (job) {
      job.state = "error";
      job.error = msg.error;
    } else {
      console.error("VidGrab host error:", msg.error);
    }
  }
  updateBadge();
  broadcast();
}

// ---------- jobs ----------

let nextId = 1;

// "Some Video - SiteName" -> "Some Video": drop a trailing segment that
// looks like the site's name, so it doesn't end up in every file name.
function cleanPageTitle(title, pageUrl) {
  title = (title || "").trim();
  let host = "";
  try { host = new URL(pageUrl).hostname.replace(/^www\./, "").toLowerCase(); } catch {}
  const squash = (s) => s.toLowerCase().replace(/[^a-z0-9]/g, "");
  const site = squash(host.split(".").slice(0, -1).join("") || host);
  for (const sep of [" - ", " | ", " — ", " – ", " :: ", " · "]) {
    const i = title.lastIndexOf(sep);
    if (i <= 0) continue;
    const tail = squash(title.slice(i + sep.length));
    if (tail && site && (site.includes(tail) || tail.includes(site))) return title.slice(0, i).trim();
  }
  return title;
}

async function startDownload({ url, preset, referer, title, nameFromPage }) {
  const opts = await getOptions();
  const id = `${Date.now()}-${nextId++}`;
  // A raw stream URL has no title of its own (yt-dlp would name the file
  // after the stream, e.g. "master"), so use the page's title instead.
  const pageTitle = nameFromPage ? cleanPageTitle(title, referer || url) : undefined;
  const job = {
    id, url, preset: preset || opts.defaultPreset, title: pageTitle || title || url, fixedTitle: !!pageTitle,
    state: "queued", percent: 0, speed: "", eta: "", stage: "Starting…",
    created: Date.now(),
  };
  jobs.set(id, job);
  broadcast();
  // Wisp keeps cookies in memory, so the helper gets this site's cookies
  // with the job (used only if the site asks for a login, unless "always").
  let cookies;
  if (opts.useCookies !== "off") {
    try { cookies = await wisp.cookies.netscape(url); } catch {}
  }
  const ok = await postToHost({ action: "download", id, url, preset: job.preset, referer, pageTitle, userAgent: navigator.userAgent, options: opts,
    cookies,
  });
  if (!ok) {
    job.state = "error";
    job.error = hostStatus?.error || "Native host unavailable";
  }
  updateBadge();
  broadcast();
  return id;
}

async function notifyJob(job) {
  const opts = await getOptions();
  if (!opts.notify) return;
  wisp.notify(
    job.state === "done" ? "Download finished" : "Download failed",
    job.state === "done" ? job.title : `${job.title}\n${job.error || ""}`,
  );
}

// The toolbar button shows how many downloads are running, or a dot when
// the current tab has playable streams.
let currentTab = null;
function updateBadge() {
  const active = [...jobs.values()].filter((j) => j.state === "running" || j.state === "queued").length;
  const streams = currentTab != null && sniffed.get(currentTab)?.size;
  wisp.action.setBadge(active ? String(active) : streams ? "●" : "");
}

// ---------- media sniffing (for sites yt-dlp doesn't know) ----------

const MEDIA_TYPES = /^(video\/|audio\/|application\/(vnd\.apple\.mpegurl|x-mpegurl|dash\+xml))/i;
const MANIFEST_EXT = /\.(m3u8|mpd)(\?|$)/i;
const FILE_EXT = /\.(mp4|webm|mkv|mov|m4v|mp3|m4a|ogg|opus|flac)(\?|$)/i;
const SEGMENT_EXT = /\.(ts|m4s|aac|vtt)(\?|$)|\/(seg|segment|chunk|frag)[-_]?\d+/i;
// Sites where yt-dlp handles the page URL itself; their raw streams are noise.
const IGNORE_HOSTS = /(googlevideo\.com|youtube\.com|ytimg\.com|ggpht\.com)$/i;

// Wisp reports video and audio responses ("media" permission).
wisp.on("media", (d) => {
  let host;
  try { host = new URL(d.url).hostname; } catch { return; }
  if (IGNORE_HOSTS.test(host) || SEGMENT_EXT.test(d.url)) return;
  const ctype = (d.mime || "").split(";")[0].trim();
  const isManifest = MANIFEST_EXT.test(d.url) || /mpegurl|dash\+xml/i.test(ctype);
  const isFile = FILE_EXT.test(d.url) || (MEDIA_TYPES.test(ctype) && !/mp2t|iso.segment/i.test(ctype));
  if (!isManifest && !isFile) return;
  const size = d.size > 0 ? d.size : 0;
  // Tiny files are usually ad pings, previews or audio cues.
  if (!isManifest && size && size < 300 * 1024) return;

  let tabMedia = sniffed.get(d.tabId);
  if (!tabMedia) sniffed.set(d.tabId, (tabMedia = new Map()));
  const key = d.url.split("#")[0];
  if (tabMedia.has(key)) return;
  tabMedia.set(key, {
    url: key,
    kind: isManifest ? (/mpd|dash/i.test(d.url + ctype) ? "DASH" : "HLS") : (ctype || "file"),
    size,
    referer: d.pageUrl || "",
    time: Date.now(),
  });
  currentTab = d.tabId;
  updateBadge();
  broadcast();
});

wisp.on("tab-updated", ({ tabId }) => {
  sniffed.delete(tabId);
  updateBadge();
});
wisp.on("tab-closed", ({ tabId }) => sniffed.delete(tabId));

// ---------- context menus ----------

// Entries are declared in plugin.json.
wisp.on("context-menu", (info) => {
  if (info.id === "page") {
    // For a <video> with a real (non-blob) src, prefer the page URL anyway:
    // yt-dlp extracts better quality from the page than from the element.
    startDownload({ url: info.pageUrl, title: info.pageTitle });
  } else if (info.id === "link" && info.linkUrl) {
    startDownload({ url: info.linkUrl, referer: info.pageUrl });
  } else if (info.id === "link-audio" && info.linkUrl) {
    startDownload({ url: info.linkUrl, referer: info.pageUrl, preset: "audio_mp3" });
  }
});

// ---------- popup / options messaging ----------

function snapshot(tabId) {
  return {
    hostStatus,
    jobs: [...jobs.values()].sort((a, b) => b.created - a.created),
    media: tabId != null ? [...(sniffed.get(tabId)?.values() || [])] : [],
  };
}

function broadcast() {
  wisp.runtime.broadcast({ type: "state-changed" }).catch(() => {});
}

// Show (or reveal the folder of) a finished file with the system's apps.
function openPath(path, reveal) {
  if (reveal && path && /\.[a-z0-9]{2,5}$/i.test(path)) path = path.replace(/\/[^/]*$/, "");
  return wisp.openPath(path || "");
}

wisp.onMessage(async (msg) => {
  switch (msg.type) {
    case "get-state":
      // The helper starts only once VidGrab is first used.
      if (!hostStatus) await refreshStatus();
      if (msg.tabId != null) currentTab = msg.tabId;
      updateBadge();
      return snapshot(msg.tabId);
    case "download":
      return startDownload(msg);
    case "cancel":
      postToHost({ action: "cancel", id: msg.id });
      return true;
    case "remove-job":
      jobs.delete(msg.id);
      broadcast();
      return true;
    case "clear-finished":
      for (const [id, j] of jobs) if (!["running", "queued"].includes(j.state)) jobs.delete(id);
      broadcast();
      return true;
    case "open":
      openPath(msg.path, msg.reveal);
      return true;
    case "browser-download":
      await wisp.downloads.start(msg.url);
      return true;
    case "refresh-status":
      hostStatus = null;
      await refreshStatus();
      return true;
    case "install-ytdlp":
      hostStatus = Object.assign({}, hostStatus, { updateOutput: "Downloading yt-dlp…" });
      broadcast();
      postToHost({ action: "install" });
      return true;
    case "update-ytdlp": {
      const opts = await getOptions();
      hostStatus = Object.assign({}, hostStatus, { updateOutput: "Updating…" });
      broadcast();
      postToHost({ action: "update", ytdlpPath: opts.ytdlpPath });
      return true;
    }
  }
});

