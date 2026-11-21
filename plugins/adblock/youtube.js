// YouTube's video ads. They can't be blocked by address: they come from the
// same servers as the videos. The player learns which ads to play from the
// video's data (adPlacements, adSlots, playerAds), which arrives three ways:
// in the page as `ytInitialPlayerResponse`, and from /youtubei/v1/player
// through fetch, read with JSON.parse or Response.json(). Those entries are
// removed before the player sees them, as uBlock Origin's json-prune and
// set-constant scriptlets do. YouTube's "ad blockers aren't allowed" box
// comes in the same data, and goes the same way.
//
// Runs in the page's own world, before YouTube's scripts, so its hooks are
// the ones YouTube calls. Anything that still gets through (an ad YouTube
// sends another way) is fast-forwarded and skipped.
(() => {
  if (window.__wispYouTube) return;
  Object.defineProperty(window, "__wispYouTube", { value: true });

  const AD_KEYS = ["adPlacements", "adSlots", "playerAds", "adBreakHeartbeatParams"];

  // Removes the ad entries from player data, wherever it sits.
  const prune = (o) => {
    if (!o || typeof o !== "object") return o;
    try {
      for (const r of [o, o.playerResponse, o.response]) {
        if (!r || typeof r !== "object") continue;
        for (const k of AD_KEYS) if (k in r) delete r[k];
        const aux = r.auxiliaryUi && r.auxiliaryUi.messageRenderers;
        if (aux && aux.enforcementMessageViewModel) delete aux.enforcementMessageViewModel;
      }
      // The watch page's own data comes as an array of parts.
      if (Array.isArray(o)) for (const part of o) if (part && part.playerResponse) prune(part);
    } catch (_) {}
    return o;
  };

  // JSON.parse: only objects that look like player data are touched, so
  // the check is a few property lookups for everything else.
  const parse = JSON.parse;
  const wrappedParse = function (...args) {
    const o = parse.apply(this, args);
    if (o && typeof o === "object" && ("playerResponse" in o || "adPlacements" in o || "playerAds" in o || "adSlots" in o || Array.isArray(o))) prune(o);
    return o;
  };
  // Looks like the original to the page (YouTube checks).
  Object.defineProperty(wrappedParse, "name", { value: "parse" });
  wrappedParse.toString = () => "function parse() { [native code] }";
  JSON.parse = wrappedParse;

  const json = Response.prototype.json;
  Response.prototype.json = function (...args) {
    return json.apply(this, args).then(prune);
  };

  // XMLHttpRequest with responseType "json" parses without JSON.parse.
  const response = Object.getOwnPropertyDescriptor(XMLHttpRequest.prototype, "response");
  if (response && response.get) {
    Object.defineProperty(XMLHttpRequest.prototype, "response", {
      configurable: true,
      enumerable: response.enumerable,
      get() {
        const r = response.get.call(this);
        if (this.responseType === "json") prune(r);
        return r;
      },
    });
  }

  // `var ytInitialPlayerResponse = {...}` in the page assigns through this.
  for (const name of ["ytInitialPlayerResponse", "playerResponse"]) {
    let value;
    try {
      Object.defineProperty(window, name, {
        configurable: true,
        get: () => value,
        set: (v) => { value = prune(v); },
      });
    } catch (_) {}
  }

  // ---- what still gets through ----

  const SKIP = ".ytp-skip-ad-button, .ytp-ad-skip-button, .ytp-ad-skip-button-modern, .ytp-ad-skip-button-slot button";
  let wasMuted = null;
  const handle = (player) => {
    const video = player.querySelector("video");
    if (!video) return;
    if (player.classList.contains("ad-showing")) {
      if (wasMuted === null) wasMuted = video.muted;
      video.muted = true;
      video.playbackRate = 16;
      // The ad's length may not be known yet: tried again each second.
      if (Number.isFinite(video.duration) && video.duration > 0) video.currentTime = video.duration;
      if (video.paused) video.play().catch(() => {});
      for (const b of player.querySelectorAll(SKIP)) b.click();
    } else if (wasMuted !== null) {
      video.muted = wasMuted;
      video.playbackRate = 1;
      wasMuted = null;
    }
  };
  // Only the player's class list is watched, not the whole page. Players
  // come and go as YouTube moves between pages without loading one, so
  // new ones are looked for once a second (two lookups, nothing more).
  const watched = new WeakSet();
  const watch = () => {
    for (const player of document.querySelectorAll("#movie_player, .html5-video-player")) {
      if (watched.has(player)) continue;
      watched.add(player);
      new MutationObserver(() => handle(player)).observe(player, { attributes: true, attributeFilter: ["class"] });
      handle(player);
    }
  };
  document.addEventListener("DOMContentLoaded", watch);
  setInterval(() => {
    watch();
    for (const player of document.querySelectorAll("#movie_player.ad-showing")) handle(player);
  }, 1000);
})();
