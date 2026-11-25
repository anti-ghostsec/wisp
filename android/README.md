# Wisp for Android

Wisp on GeckoView (Gecko, Firefox's engine) instead of WebKitGTK, with the
same Rust core as the desktop browser (`../core`, wisp-core): its settings,
privacy rules, vault, history, sync and the plugin API's rules are one code
base on both.

```
core/            wisp-core: no GTK, no WebKit (shared with the desktop browser)
android/rust/    wisp-ffi: wisp-core on a thread of its own, bindings for Kotlin
                 (UniFFI); wisp-extensions, which turns plugins/ into extensions
android/app/     the app (Kotlin): GeckoView, tabs, menus, the plugin bridge
```

## Build

Needs the Android SDK with the NDK, a JDK (17 or newer), Rust with the
Android targets, and cargo-ndk, all of which can live in your home folder:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
export ANDROID_HOME=~/.local/opt/android-sdk     # the default
sdkmanager "ndk;29.0.14206865" "platforms;android-37.2"
android/build-android.sh arm64-v8a              # phones; x86_64 for the emulator
```

The APK is in `android/app/build/outputs/apk/debug/`. `build-android.sh ABI
release` makes an unsigned release build, for signing with `apksigner`.

Every push to `main` also puts two signed APKs into the release (the
`android` job in `.github/workflows/release.yml`), numbered by commit count so
each installs over the one before: `wisp-android-arm64.apk` for phones, and
`wisp-android-x86_64.apk` for the Android emulator (an x86-64 system image),
each with its `.sha256`. It's signed with Wisp's
Android key, kept out of the repository (GitHub secrets `ANDROID_KEYSTORE`
and `ANDROID_KEYSTORE_PASSWORD`); the workflow checks the signature against
`android/release-cert.sha256`. Android only updates an app with the key it was
installed with: lose the key and the next APK can't replace the installed one.

GeckoView comes from Mozilla's Maven repository, one package per CPU type
(about 90 MB). Its version is in `app/build.gradle.kts`; Mozilla publishes a
new one with every Firefox release (every 4 weeks), and that's how Gecko's
security fixes reach Wisp, so move to each one.

## Privacy, as on desktop

- **Private session**: every tab is a private GeckoView session; cookies,
  cache and site storage go when Wisp does. *Keep cookies and site data* keeps
  them (`persistent_profile`).
- **HTTPS-only**: Gecko's own HTTPS-only mode, for pages and everything they
  load. If HTTPS fails, Gecko's page lets you continue over HTTP for that site,
  for this session. Local addresses (localhost, private IPs) and `.onion` stay
  on HTTP. Unlike desktop Wisp, `.local` and `.test` names are upgraded too.
- **Tracking parameters** (`utm_*`, `fbclid`, ...) are taken off addresses
  before they're loaded (wisp-core's list), and Gecko's own query stripping is on.
- **Cookies and tracking**: anything a page embeds gets a cookie jar of its
  own for that site, and trackers get none; Gecko blocks ad, analytics, social,
  fingerprinting and cryptomining trackers; Global Privacy Control sent. Not
  Gecko's "content" trackers: those include Stripe's card forms and
  Cloudflare's "are you human" checks, which sites can't work without. Safe
  Browsing (which asks Google about pages) is off.
- **Ad & tracker blocking**: EasyList, EasyPrivacy and uBlock's badware list,
  about 110k rules, as declarativeNetRequest rules Gecko runs natively. Gecko
  takes 30,000 rules from one extension, so the plugin's rules are spread over
  `adblock` and `adblock+2`... `adblock+4`, turned on and off together. The lists
  are fetched when the app is built (kept for 4 days in `android/build/lists`):
  they're as fresh as the build.
- **Off**: WebGL, WebRTC (also no IP leak through it), WebGPU, asm.js,
  Graphite and SVG-in-OpenType fonts, Gecko's prefetching, `<a ping>`, its
  captive-portal and connectivity checks.
- **Security level** (☰ → *Security level*), like Tor Browser's:
  - *Standard*: everything else works; WebGL and WebRTC as set in Settings.
  - *Safer* (the default): no JIT compilers (most browser exploits go through
    them), so no WebAssembly either; no SharedArrayBuffer, WebGL, WebRTC or
    MathML. Heavy sites and web apps are slower.
  - *Safest*: also no JavaScript at all, and only the phone's own fonts.
- **Site isolation** (Gecko's Fission): each site gets content processes of
  its own, so a page that takes over its process still can't read another
  site's. They don't run as Android *isolated processes* (a tighter
  sandbox): in those, Gecko's release builds can't play most video.
- **Fingerprinting**: Gecko's fingerprinting protection, and the
  `fingerprint-shield` plugin.
- **Permissions**: location, camera, microphone, notifications: denied.
- **Pop-ups**: held by Gecko's blocker, shown in the bar with **Open** /
  **Allow on this site** (kept in memory until Wisp closes, never on disk).
- **App links** (`mailto:`, `magnet:`, ...) open in another app after asking.
- **Route** (☰ → *Route*):
  - *Direct*: straight out, through your VPN if one is on.
  - *Tor (built in)*: Tor runs inside Wisp (the Guardian Project's
    tor-android, the tor Orbot runs). Everything (pages, the sync service)
    goes through it, host names looked up through Tor too. Until it's
    connected Gecko is pointed at a port nothing listens on: nothing loads,
    and nothing ever goes around it. Tor listens on a Unix socket in Wisp's
    own folder, not a port: other apps on the phone can't use it (or see
    what goes through it), and the route's settings are in place before any
    page or plugin loads.
    All sites share Tor's circuits: GeckoView has no way to give each site
    its own, as Tor Browser does (their cookies and storage are still kept
    apart).
  - *Orbot*: through the Orbot app's SOCKS port (9050) instead, the same
    way. Any app can listen on that port, and Wisp can't tell Orbot from
    another one, so the built-in Tor is the safer choice.
  - **Bridges** (☰ → Privacy & security → *Tor bridges*), where Tor itself
    is blocked: the Tor Project's built-in obfs4, Snowflake or meek bridges,
    or bridge lines of your own (obfs4, webtunnel, Snowflake, meek), through
    the same pluggable transports Orbot uses (IPtProxy). If the bridges'
    transport doesn't start, Tor isn't started either.
  - Changing the route asks first, then closes every tab (background ones
    too) and clears cookies, site storage and caches, so no site you had
    open sees you arrive by the other route; a new tab opens only once
    Gecko has the new route's settings. On Tor (built in or Orbot) Gecko's
    *resist fingerprinting* is on too, pages are asked for in US English,
    and the Fingerprint Shield plugin leaves Gecko's values alone.
- **Local addresses**: web pages can't reach this phone's own servers
  (localhost) or the local network, which apps use to tie your visits to
  your accounts in them.
- **Screen privacy**: no screenshots or screen recordings of Wisp, and a
  blank card in the recent apps (*Allow screenshots of Wisp* in Settings
  turns it off), dialogs included. With a Wisp password, the vault locks
  again after five minutes in the background: saved logins, bookmarks,
  history and plugin data leave memory, and every tab closes until you
  unlock it ("Not now" browses on without them).
- **Keyboard and autofill**: Wisp's own fields ask the keyboard not to learn
  what's typed (its incognito mode), and Android's autofill services (a
  phone maker's password manager among them) see neither them nor pages.
- **Copied passwords** are marked sensitive and cleared from the clipboard
  after 45 seconds.
- **Updates**: once a week while on the built-in Tor, or when you ask (☰ →
  More → *Check for updates*), Wisp looks at its newest GitHub release the
  way it loads pages (over Tor on Tor, which also gets past a block on
  GitHub). An update is installed only if its SHA-256 matches the release's
  and it's signed with the same key as the Wisp you have; Android asks
  before installing it. When the browser engine is more than 60 days old,
  Wisp says so on any route (at most once a week).
- **Duress password** (☰ → Privacy & security, with a Wisp password): typed
  where Wisp asks for its password, it wipes everything at once and closes
  Wisp, which then starts as new. Long-press Wisp's icon for **Wipe Wisp
  now**, which does the same without opening it.
- **Sealed settings**: the settings Wisp needs before it's unlocked (the
  route, Tor's bridges) and Tor's own files (its guards and directory) are
  encrypted with a key in Android's keystore, which never leaves the phone.
  Tor's files are in the clear only while Tor runs (and, if Wisp is killed
  meanwhile, until it next starts).
- **Wipe everything** (☰): every tab, the history, the vault, cookies, site
  data, downloads and settings are deleted from the phone at once, as
  Android's "Clear storage" does. What's on the sync service stays there.
- **No backups**: nothing of Wisp's goes into Android's cloud backups or
  device transfers.

## Downloads

Files are kept inside Wisp, where other apps can't see them, and *Wipe
everything* deletes them. ☰ → **Downloads** lists what's downloading, with
Cancel, and what's finished, to open, share, save to the phone (**Save…**,
wherever you pick) or delete; a file leaves Wisp only that way. While one
downloads, a notification shows its progress, never the file's name (Android
13 and later ask once whether Wisp may show notifications).

## The ☰ menu

Every day: back and forward, reload, new tab, bookmarks and passwords,
downloads, history. **Privacy & security**: the security level, the route,
the Wisp password and *Wipe everything*. **More**: the element blocker,
settings, plugins, the sync account and *Close all tabs*.

Searches other apps hand to a browser ("Search the web") open in Wisp.

## Plugins

Each plugin in `plugins/` becomes a built-in extension (Manifest V3) when the
app is built: its scripts, styles and blocklists, plus `wisp-api.js`, the
`wisp` API desktop plugins use, answered by the app over native messaging. The
calls that keep data (`storage`, `session`, `sync`) are wisp-core's, with the
desktop's rules: which page asks comes from GeckoView (its address, and
whether it's the plugin's own page), so a web page only ever sees its own
site's saved logins. A plugin's own pages open as tabs (`wisp-plugin://<id>/...`
in the address bar).

VidGrab is left out: it needs its helper program (yt-dlp), and Android apps
can't run one.

## Not yet

- Date and colour pickers in pages.
- Downloading filter-list updates while the app runs (they come with each build).
- A start page: new tabs are blank, with the address bar ready.

☰ → **Sync account…** takes your own sync ID and password (the same as on your
other devices) or makes a new account, as on desktop.
