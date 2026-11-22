// Settings shared by the background script, the popup and the settings page.
const DEFAULT_OPTIONS = {
  outputDir: "",
  filenameTemplate: "%(title).150B.%(ext)s",
  defaultPreset: "best",
  allowPlaylist: false,
  embedMetadata: true,
  subtitles: true,
  subLangs: "en.*",
  sponsorblock: false,
  // Whether yt-dlp gets your cookies for the site: "off" (unless you turn
  // it on, a download never carries your login), "auto" (retry with them
  // when a site asks for a login) or "always". (A new name: the old
  // "useCookies" was on by default, so it isn't read any more.)
  useLogin: "off",
  ytdlpPath: "",
  extraArgs: "",
  notify: true,
};

const PRESETS = [
  ["best", "Best video (MP4)"],
  ["2160", "Up to 4K"],
  ["1440", "Up to 1440p"],
  ["1080", "Up to 1080p"],
  ["720", "Up to 720p"],
  ["480", "Up to 480p"],
  ["audio_mp3", "Audio only (MP3)"],
  ["audio_m4a", "Audio only (M4A)"],
  ["audio_best", "Audio only (original)"],
];

async function getOptions() {
  const opts = Object.assign({}, DEFAULT_OPTIONS, (await wisp.storage.get("options")) || {});
  opts.useCookies = ["auto", "always"].includes(opts.useLogin) ? opts.useLogin : "off";
  return opts;
}
