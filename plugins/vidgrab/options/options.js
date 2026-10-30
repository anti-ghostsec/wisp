// The settings page. shared.js runs first (plugin.json).
/* global DEFAULT_OPTIONS, PRESETS, getOptions */

const $ = (id) => document.getElementById(id);

async function renderStatus() {
  const { hostStatus: s } = await wisp.runtime.sendMessage({ type: "get-state" });
  const out = $("status");
  if (!s) {
    out.textContent = "Checking…";
  } else if (s.error) {
    out.textContent = `The helper isn't running: ${s.error}. It needs python3; yt-dlp can be installed from the VidGrab popup.`;
  } else {
    out.textContent = [
      `Helper v${s.hostVersion}`,
      s.ytdlp ? `yt-dlp ${s.ytdlp} (${s.ytdlpPath})` : "yt-dlp NOT FOUND",
      s.ffmpeg ? `ffmpeg: ${s.ffmpeg}` : "ffmpeg NOT FOUND (needed for HD merging and MP3)",
      s.jsRuntime ? `JS runtime: ${s.jsRuntime}` : "JS runtime NOT FOUND (YouTube needs deno or node: sudo apt install nodejs)",
      "Cookies: Wisp passes the site's cookies with each download (see “Use my login” below)",
      `Default folder: ${s.defaultDir}`,
    ].join("\n");
    out.style.whiteSpace = "pre-line";
    if (s.updateOutput) {
      $("update-output").hidden = false;
      $("update-output").textContent = s.updateOutput;
    }
  }
}

async function init() {
  for (const [id, label] of PRESETS) {
    const o = document.createElement("option");
    o.value = id;
    o.textContent = label;
    $("defaultPreset").append(o);
  }
  const opts = await getOptions();
  for (const key of Object.keys(DEFAULT_OPTIONS)) {
    const input = $(key);
    if (!input) continue;
    if (input.type === "checkbox") input.checked = !!opts[key];
    else input.value = opts[key];
  }

  $("save").addEventListener("click", async () => {
    const next = {};
    for (const key of Object.keys(DEFAULT_OPTIONS)) {
      const input = $(key);
      if (!input) continue;
      next[key] = input.type === "checkbox" ? input.checked : input.value.trim();
    }
    await wisp.storage.set("options", next);
    $("saved").textContent = "Saved.";
    setTimeout(() => ($("saved").textContent = ""), 2000);
    await wisp.runtime.sendMessage({ type: "refresh-status" });
  });
  $("recheck").addEventListener("click", () => wisp.runtime.sendMessage({ type: "refresh-status" }));
  $("update").addEventListener("click", () => wisp.runtime.sendMessage({ type: "update-ytdlp" }));

  wisp.on("broadcast", (msg) => {
    if (msg.type === "state-changed") renderStatus();
  });
  renderStatus();
}

init();
