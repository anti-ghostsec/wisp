# Wisp

A small, privacy-focused web browser built on WebKitGTK 6 and GTK 4, written in Rust.

## Build

```sh
sudo apt install libwebkitgtk-6.0-dev libgtk-4-dev pkg-config   # Debian/Ubuntu
cargo build --release
./target/release/wisp [URL...]
```

## Privacy defaults

- **Private session**: an ephemeral network session, so cookies, cache and
  site storage never touch the disk. Set `persistent_profile: true` to keep them.
- **HTTPS-only**: http:// pages are upgraded. If HTTPS fails, a warning page
  lets you continue over HTTP for that host, for this session only.
- **Tracking parameters** (`utm_*`, `fbclid`, `gclid`, …) are removed from URLs.
- **Cookies and tracking**: third-party cookies are blocked, and WebKit's
  Intelligent Tracking Prevention is on.
- **Ad & tracker blocking** (the `adblock` plugin): EasyList, EasyPrivacy and
  uBlock's badware list, about 110k rules, compiled into WebKit's native
  content blocker. Navigating or redirecting to a listed ad or scam domain is
  blocked too.
- **Pop-up guard**: a script pop-up to another site (the usual click-hijack or
  pop-under trick) is held behind a bar with **Open** / **Always allow on this
  site**. Normal `target=_blank` links open as tabs.
- **Disabled features**: WebGL, WebRTC, WebKit's own credential storage and
  script popups are off. The ad blocker also blocks third-party `<a ping>`
  beacons.
- **Permissions**: geolocation, camera, microphone and notification requests
  are denied.
- **Fingerprinting**: the `fingerprint-shield` plugin reports uniform device values.
- **Search**: the default engine is DuckDuckGo's HTML version.

## Low memory use

- **One process**: windows share a single UI process, and tabs share one web
  process where WebKit allows it (`shared_web_process`).
- **Tab suspension**: background tabs unload after `suspend_tabs_after_minutes`
  of inactivity. They keep their history and reload when you switch back.
- **Memory limit**: each web process has a soft limit (`memory_limit_mb`).
  Above it, WebKit drops caches and runs garbage collection.
- **Small caches**: the cache model is small and there is no back/forward page
  cache (`low_memory_cache`).
- **Rendering**: page content is drawn by the CPU and the browser UI with
  cairo, which keeps text crisp and keeps GPU drivers out of the UI process.
  Finished frames are composited on the GPU (`gpu_compositing`, on by
  default) for smooth scrolling and video. That costs ~100 MB in the web
  process; turn it off for pure software rendering. Wisp composites on the
  GPU that drives your display (handy on laptops and hybrid desktops) and
  passes frames through shared memory, because DMA-BUF hand-off hangs on
  some drivers. `hardware_acceleration` also moves page drawing and the UI
  to the GPU. Environment variables you set yourself (`GSK_RENDERER`,
  `WEBKIT_*`) take precedence.
- **Native blocklists**: content blocking is compiled bytecode, not JavaScript.

## Bookmarks & passwords

The `bookmarks` (Ctrl+D, Ctrl+Shift+O) and `passwords` plugins sync through
xBrowserSync, end-to-end encrypted. You don't need to set anything up: an
account is created the first time you save something. To use your own sync
ID and password, open ☰ → **Sync account…**.

Wisp keeps an encrypted copy of your synced data in
`~/.local/share/wisp/sync-cache.json`, so logins and bookmarks are available
instantly. Changes are saved locally first and uploaded in the background;
if you're offline, they're retried until they go through.

**Passwords**: a key button appears in password fields (and in username
fields on sites with saved logins). It lists the saved logins for the site,
offers **Generate strong password**, and links to the password manager
(the key in the toolbar), where you can view, edit, add and delete logins.
Right-clicking a text field also offers **Fill saved login…** and **Generate
strong password**. A lone password field while you're logged in (a "confirm
your password" prompt) is never filled automatically or offered for saving.

**Importing**: the password manager imports CSV or JSON exports from Firefox,
Chrome, Edge, Brave, Safari, Proton Pass, Bitwarden and KeePassXC (export
them unencrypted). The bookmarks page imports the `bookmarks.html` file
every browser can export, or Chrome's `Bookmarks` file.

## Downloads

Wisp asks where to save each download, starting in your Downloads folder.
To skip the question, set `"ask_download_location": false`; files then go to
`download_dir` (your Downloads folder when null), and a name that's taken
gets a number, e.g. `photo (2).png`. The download button next to the address
bar shows progress, turns blue when a download finishes, and lists this
session's downloads: click one to open it; the folder button shows where it is.

Files and folders open with whatever your system has, tried in this order:
the desktop's default app for the type, the `org.freedesktop.FileManager1`
service (folders), `extension = command %f` lines in
`~/.config/ferro/associations.conf` or `/etc/ferro/associations.conf`, any
installed app that declares the type, and finally common file managers and
text editors found on `PATH`. If all of that fails, images, PDFs and text
open in a Wisp tab, and anything else shows its path with **Copy path**.

## History, tabs and the bookmarks bar

All optional, from the ☰ menu:

- **Show bookmarks bar** (Ctrl+Shift+B): your bookmarks under the address
  bar. Bookmarks that don't fit are under » at the end (or scroll the bar
  with the mouse wheel). Click to open, middle-click for a new tab, drag one
  along the bar (or within the » menu) to move it, right-click to open it in
  a new tab, move it to the start or delete it. The bookmarks page
  (Ctrl+Shift+O) lists them in the same order: drag the ⠿ handle to
  reorder, or edit names and addresses. The order syncs with the bookmarks.
- **Remember history** (off by default): Ctrl+H shows it, grouped by day and
  searchable. Turning it off deletes it.
- **Reopen tabs on start** (off by default): your windows and tabs come back
  next time; tabs other than the first in each window load when you open
  them. Turning it off deletes the saved tabs.

History and tabs are stored encrypted (AES-256-GCM). The key is kept
separately, in `~/.config/wisp/state.key`, so the data folder on its own (a
backup, a copied folder) reveals nothing. Someone who can read your files
as you can read both; full-disk encryption covers that case.
**Clear cookies, site data and history** in the menu wipes them along with
cookies and site storage.

## What touches the disk

With the default private session, pages leave nothing behind: cookies,
cache, history and site storage live in memory and vanish when Wisp quits.
What Wisp does write (all readable only by you):

| Path | What |
|---|---|
| `~/.config/wisp/config.json` | Settings |
| `~/.config/wisp/sync.json` | Sync ID and password, in plain text, once sync is used |
| `~/.local/share/wisp/sync-cache.json` | Encrypted copy of synced bookmarks and passwords, plus its key |
| `~/.local/share/wisp/history.enc`, `session.enc` | History and open tabs, encrypted, only when turned on |
| `~/.config/wisp/state.key` | Their key |
| `~/.local/share/wisp/plugin-data/` | What plugins store with `wisp.storage` (e.g. sites you chose "Never" for) |
| `~/.cache/wisp/` | Ad-block filter lists and their compiled form |
| Your download folder | Files you download |

Libraries Wisp uses keep their own caches: GStreamer's plugin list
(`~/.cache/gstreamer-1.0`) and the GPU driver's shader cache
(`~/.cache/mesa_shader_cache*`). Neither holds browsing data. Setting
`persistent_profile: true` stores cookies and site data under
`~/.local/share/wisp/profile` and `~/.cache/wisp/profile`.

## Video

Video playback uses GStreamer. On Debian/Ubuntu:

```sh
sudo apt install gstreamer1.0-libav gstreamer1.0-plugins-bad gstreamer1.0-gl
```

Wisp draws its own toolbar icons, so they show even without an SVG loader.
Other GTK icons (dialogs, file chooser) need `librsvg2-common`.

## Settings

`~/.config/wisp/config.json` is created on first run. Edit it in ☰ →
**Settings…**, which checks the text and points out mistakes (including
misspelled setting names) before saving. Restart Wisp (Ctrl+Q) to apply.

## Shortcuts

| Keys | Action |
|---|---|
| Ctrl+T / Ctrl+W / Ctrl+N | New tab / close tab / new window |
| Ctrl+Q | Quit Wisp (all windows) |
| F11 | Fullscreen (hides the toolbar and tabs) |
| Ctrl+H | History |
| Ctrl+Shift+B | Show or hide the bookmarks bar |
| Ctrl+L, F6 | Focus address bar |
| Ctrl+R, F5 | Reload |
| Alt+← / Alt+→ | Back / forward |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Ctrl+F | Find in page |
| Ctrl+= / Ctrl+- / Ctrl+0 | Zoom |
| Ctrl+Shift+I, F12 | Web Inspector |
| Ctrl+D / Ctrl+Shift+O | Bookmark page / show bookmarks |
| + after the last tab, or double-click the empty tab bar | New tab |
| Drag a tab | Move it |
| Middle-click / Ctrl+click on a link | Open in background tab |

## Plugins

Turn plugins on and off in the ☰ menu. Drag a plugin by its ⠿ handle to
change where its buttons sit in the toolbar, and untick **Toolbar** to keep
its buttons out of the toolbar (they then appear next to it in the menu).

**VidGrab** downloads videos from YouTube and the 1000+ other sites
[yt-dlp](https://github.com/yt-dlp/yt-dlp) supports: use its toolbar
button, or right-click a page, video or link. On other sites it spots the
video streams a page plays (start the video first). yt-dlp sets itself up
the first time you use VidGrab (a few MB, into `~/.local/share/vidgrab`) and
checks for updates weekly, since sites change and yt-dlp follows within
days; a yt-dlp already on `PATH` is used as is. It needs `python3`. With
`ffmpeg` (`sudo apt install ffmpeg`) you also get HD (sites send video and
audio separately above 720p), MP3/M4A conversion and embedded metadata;
without it, VidGrab downloads the best single-file version. When a site asks for a login,
yt-dlp gets your cookies for that site in a private temporary file that's
deleted when the download ends.

See [PLUGINS.md](PLUGINS.md) to write your own.
