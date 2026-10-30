// Settings shared by the background script, the popup and the settings page.
const DEFAULT_OPTIONS = {
  outputDir: "",
  filenameTemplate: "%(title).150B [%(id).30B].%(ext)s",
  defaultPreset: "best",
  allowPlaylist: false,
  embedMetadata: true,
  subtitles: false,
  subLangs: "en.*",
  sponsorblock: false,
  useCookies: "auto", // "off" | "auto" (retry when a site asks for a login) | "always"
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
  return Object.assign({}, DEFAULT_OPTIONS, (await wisp.storage.get("options")) || {});
}
