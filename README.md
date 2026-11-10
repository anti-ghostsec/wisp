# Wisp

A small, privacy-focused web browser built on WebKitGTK 6 and GTK 4, written in Rust.

## Build

```sh
sudo apt install libwebkitgtk-6.0-dev libgtk-4-dev pkg-config   # Debian/Ubuntu
cargo build --release
./target/release/wisp [URL...]
```

Every push to `main` publishes a ready-built Linux (x86-64) Wisp as the
newest [release](https://github.com/anti-ghostsec/wisp/releases/latest),
built on Debian trixie (`.github/build-linux.sh`). FerroOS's updater installs
it from there.

## Privacy defaults

- **Private session**: an ephemeral network session, so cookies, cache and
  site storage never touch the disk. Set `persistent_profile: true` to keep them.
- **HTTPS-only**: http:// pages and frames are upgraded before anything is
  sent, sites given as a public IP address included; only this computer,
  the local network (private and link-local addresses) and `.local`,
  `.onion`, `.test` and `localhost` names stay on HTTP. If HTTPS fails, a
  warning page lets you continue over HTTP for that host and port, for this
  session only. Only that page's own button can do so: a web page can't
  allow HTTP for another site.
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
- **Safer JavaScript** (☰ → *Safer JavaScript (slower)*, `no_jit`, off by
  default): scripts run in JavaScriptCore's interpreter only, without the
  JIT compiler that most browser exploits go through. Heavy sites and web
  apps get slower. It takes effect when Wisp next starts, and a `JSC_useJIT`
  you set yourself takes precedence.
- **Site databases** (IndexedDB), which many sites and web apps need, are
  allowed: in the private session they stay in memory and are gone when Wisp
  quits. Untick **Allow site databases** in the ☰ menu to turn them off.
- **App links** (`claude://`, `mailto:`, `magnet:`, …), such as a sign-in
  page handing you back to its app, open in the app your system has for
  them, after asking. **Always open** skips the question for that kind of
  link (stored in `allowed_app_links`).
- **Files and links from other apps** open as new tabs in the Wisp window
  you're using, not as another browser window.
- **Permissions**: geolocation, camera, microphone and notification requests
  are denied.
- **Fingerprinting**: the `fingerprint-shield` plugin reports uniform device values.
- **Search**: the default engine is DuckDuckGo with its ads turned off
  (`k1=-1`); its upgrade pop-over and feedback button are hidden. Type
  `ai` and a question in the address bar to ask Duck.ai instead.

## Low memory use

- **One process**: windows share a single UI process, and tabs share one web
  process where WebKit allows it (`shared_web_process`). Plugins share a
  second one, so a page that's busy loading doesn't hold up their buttons.
- **Tab suspension**: background tabs unload after `suspend_tabs_after_minutes`
  of inactivity. They keep their history and reload when you switch back.
- **Background tabs wait**: a link opened in the background (middle-click,
  Ctrl+click, Open in New Tab) loads only when you first switch to it, so
  opening several videos at once doesn't load and buffer them all together.
- **Playing tabs keep playing**: WebKit pauses a page's video and music
  when its tab is hidden; a tab that's playing sound when you leave it is
  kept going, unseen, and goes back to normal when you return or a little
  after it stops.
- **Memory limit**: each web process has a soft limit (`memory_limit_mb`).
  Above it, WebKit drops caches and runs garbage collection.
- **Small caches**: the cache model is small (WebKit's document browser: it
  still keeps pictures, so pages that redraw their lists don't fetch them
  again and flash) and there is no back/forward page cache
  (`low_memory_cache`).
- **Rendering**: page content is drawn by the CPU and the browser UI with
  cairo, which keeps text crisp and keeps GPU drivers out of the UI process.
  Finished frames are composited on the GPU (`gpu_compositing`, on by
  default) for smooth scrolling and video. That costs ~100 MB in the web
  process; turn it off for pure software rendering. Wisp composites on the
  GPU that drives your display (handy on laptops and hybrid desktops) and
  passes frames through shared memory, because DMA-BUF hand-off hangs on
  some drivers. `hardware_acceleration` also moves page drawing and the UI
  to the GPU. Video frames reach the page on the GPU too (`gpu_video`, on
  by default; ☰ → *Draw video on the graphics card*). Some drivers (seen
  with Intel's iris) show some sites' videos black that way though the
  frames are fine: untick it and they arrive as plain pictures, for a
  little more CPU. Environment variables you set yourself (`GSK_RENDERER`,
  `WEBKIT_*`) take precedence.
- **Dark browser** (☰ → *Dark browser (pages unchanged)*, `dark_ui`): Wisp's
  own toolbar, tabs and menus in GTK's dark style. Web pages aren't told and
  nothing is inverted, so sites look as they always do (the Dark Mode
  plugin, which asks sites for their dark theme and inverts the rest, is
  still there for those who want pages dark too).
- **Big copies**: GTK 4 hangs handing over a copy bigger than 256 KB on
  X11 (Ctrl+A, Ctrl+C on most pages, with its formatting), and freezes
  Wisp and whatever pastes from it. Past 50 000 characters Wisp copies the
  plain text itself, in pieces as X11 expects; smaller copies keep their
  formatting.
- **Native blocklists**: content blocking is compiled bytecode, not JavaScript.

## Bookmarks & passwords

The `bookmarks` (Ctrl+D, Ctrl+Shift+O) and `passwords` plugins sync through
xBrowserSync, end-to-end encrypted. You don't need to set anything up: an
account is created the first time you save something. To use your own sync
ID and password, open ☰ → **Sync account…**. A sync service must be reached
over `https://` (one on this computer, or a `.onion` address, may be
`http://`).

Wisp keeps an encrypted copy of your synced data in
`~/.local/share/wisp/sync-cache.json`, so logins and bookmarks are available
instantly. Changes are saved locally first and uploaded in the background;
if you're offline, they're retried until they go through.

**Passwords**: a key button appears in password fields (and in username
fields on sites with saved logins). It lists the saved logins for the site
(the exact host they were saved on, `www.` aside: a login for
`one.example.io` is never offered on `two.example.io`),
offers **Generate strong password** (the most recently used login first,
then the newest saved), and links to the password manager
(the key in the toolbar), where you can view, edit, add and delete logins.
Right-clicking a text field also offers **Fill saved login…** and **Generate
strong password**. A lone password field while you're logged in (a "confirm
your password" prompt) is never filled automatically or offered for saving.

Local servers work too, over plain HTTP: `localhost`, `*.localhost`,
`*.test` and IP addresses (a dev server, your router), each address and port
with its own logins. Other plain-HTTP sites are left alone, since a password
sent there isn't encrypted.

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
  to move it: along the bar, within the » menu, or between the two (hold it
  over » to open the menu). New bookmarks (Ctrl+D, or a tab dropped on the
  bar's empty part) go first. Right-click to open it in a new tab, move it to
  the start or delete it. The bookmarks page
  (Ctrl+Shift+O) lists them in the same order: drag the ⠿ handle to
  reorder, or edit names and addresses. The order syncs with the bookmarks.
- **Remember history** (off by default): Ctrl+H shows it, grouped by day and
  searchable. Turning it off deletes it.
- **Reopen tabs on start** (off by default): your windows and tabs come back
  next time; tabs other than the first in each window load when you open
  them. Turning it off deletes the saved tabs.

History and tabs are stored encrypted (AES-256-GCM), like everything else
Wisp keeps; see [Wisp password](#wisp-password).
**Clear cookies, site data and history** in the menu wipes them along with
cookies and site storage.

## Wisp password

The first time it starts, Wisp asks for a password. It seals
`~/.config/wisp/vault.json`, which holds the key everything Wisp keeps is
encrypted with (saved passwords, bookmarks, history, tabs, plugin data) and
your sync login; the sealing key is made from the password with Argon2id
(64 MiB, 3 passes). Wisp asks for it once each time it starts. Without it
none of those files can be read, even by someone with a copy of your home
folder; it can't be recovered if you forget it.

"Not now" starts Wisp locked: nothing is recorded or saved, and bookmarks
and passwords can't be used until you unlock it (☰ → Wisp password…), which
is also where you change or remove the password. Without a password the
vault is sealed with a key kept beside it, which only stops it being read
by accident.

Wisp notes (in `~/.local/share/wisp/vault-protection`) whether the vault has
a password. If `vault.json` turns up without one though you never removed it
in Wisp, the file was replaced: Wisp says so when it starts and stays locked
unless you choose to go on without a password. Keys and passwords are wiped
from Wisp's memory when they're no longer needed.

Older Wisps kept the sync login (`sync.json`) and the history key
(`state.key`) unsealed; they're moved into the vault and wiped on the first
start.

## What touches the disk

With the default private session, pages leave nothing behind: cookies,
cache, history and site storage live in memory and vanish when Wisp quits.
What Wisp does write (all readable only by you):

| Path | What |
|---|---|
| `~/.config/wisp/config.json` | Settings |
| `~/.config/wisp/vault.json` | The key for everything below and the sync ID and password, sealed with your Wisp password |
| `~/.local/share/wisp/vault-protection` | Whether the vault has a password (`password` or `none`), to notice a replaced `vault.json` |
| `~/.local/share/wisp/sync-cache.json` | Synced bookmarks and passwords, encrypted |
| `~/.local/share/wisp/history.enc`, `session.enc` | History and open tabs, encrypted, only when turned on |
| `~/.local/share/wisp/plugin-data/` | What plugins store with `wisp.storage` (e.g. sites you chose "Never" for), encrypted |
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
| Drag a tab | Move it; drop it on the bookmarks bar to bookmark it there |
| Middle-click / Ctrl+click on a link, or right-click → Open in New Tab | Open in a background tab (you stay where you are) |

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
days; a yt-dlp already on `PATH` is used as is. What VidGrab fetches is kept
and run only if its SHA-256 is in the release's list of hashes
(`SHA2-256SUMS`) and that list carries a good signature from yt-dlp's
release key, which is built into the helper; a release older than the last
one seen is refused. It needs `python3`. With
`ffmpeg` (`sudo apt install ffmpeg`) you also get HD (sites send video and
audio separately above 720p), MP3/M4A conversion and embedded metadata;
without it, VidGrab downloads the best single-file version. When a site asks for a login,
yt-dlp gets your cookies for that site in a private temporary file that's
deleted when the download ends. Its helper (a Python program that runs
yt-dlp) starts only once you've allowed it, which Wisp asks the first time.
**Extra yt-dlp arguments** in its settings take download, format, subtitle
and file-name options only: ones that run programs or reach other files
(`--exec`, `-o`, `--config-locations`, …) are refused, and the yt-dlp path
must name a `yt-dlp` program.

**Element Blocker** hides things the ad blocker misses, such as a
newsletter pop-up or a "disable your ad blocker" wall. Nothing is blocked
until you choose it: right-click the thing and pick **Block element…** (or
press Ctrl+Shift+E), point at it, and click. Scroll or use ↑ ↓ to select
the box around it instead, or edit the selector by hand. It stays hidden on
that site from then on, and if it was an overlay that stopped the page from
scrolling, scrolling works again. **Blocked elements** (in the ☰ menu)
lists them by site to edit or remove.

A plugin you install with the same id as a built-in one doesn't replace it
unless you choose **Use Mine Instead** when Wisp asks. See
[PLUGINS.md](PLUGINS.md) to write your own.

## Releases

Every push to `main` that passes clippy (warnings count) and the tests is
built on Debian trixie and published as the newest release, signed with
minisign. What the build uses is fixed by hash (the checkout action, the
Debian image, Rust's installer) or by version (Rust), so it changes only
when the workflow does. FerroOS's updater installs a build only if its signature checks out
against [minisign.pub](minisign.pub), it is the commit announced, and it is
no older than the one installed. To check a download yourself:

```sh
minisign -Vm wisp-linux-x86_64.tar.gz -P RWS1Mw19uE/JOmQzjVNTTIz4ovqKsQOkMSWd9AU3s9+ZtFdMRe3qCd1P
```
