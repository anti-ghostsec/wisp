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

Every push to `main` also puts a signed `wisp-android-arm64.apk` into the
release (the `android` job in `.github/workflows/release.yml`), numbered by
commit count so each installs over the one before. It's signed with Wisp's
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
- **Cookies and tracking**: third-party cookies blocked; Gecko's strict
  Enhanced Tracking Protection; Global Privacy Control sent. Safe Browsing
  (which asks Google about pages) is off.
- **Ad & tracker blocking**: EasyList, EasyPrivacy and uBlock's badware list,
  about 110k rules, as declarativeNetRequest rules Gecko runs natively. Gecko
  takes 30,000 rules from one extension, so the plugin's rules are spread over
  `adblock` and `adblock+2`... `adblock+4`, turned on and off together. The lists
  are fetched when the app is built (kept for 4 days in `android/build/lists`):
  they're as fresh as the build.
- **Off**: WebGL, WebRTC (also no IP leak through it), Gecko's prefetching,
  `<a ping>`, its captive-portal and connectivity checks. *Safer JavaScript*
  turns off the JIT compilers (and WebAssembly).
- **Fingerprinting**: Gecko's fingerprinting protection, and the
  `fingerprint-shield` plugin.
- **Permissions**: location, camera, microphone, notifications: denied.
- **Pop-ups**: held by Gecko's blocker, shown in the bar with **Open** /
  **Always allow on this site**.
- **App links** (`mailto:`, `magnet:`, ...) open in another app after asking.
- **Content processes** run as Android *isolated processes*.
- **Routes**: straight out, through your VPN if one is on, or
  **Route through Orbot (Tor)** in Settings: everything (pages, the sync
  service) goes through Orbot's SOCKS port with host names looked up through
  Tor. If Orbot isn't running, nothing loads: Wisp never goes around it.
- **No backups**: nothing of Wisp's goes into Android's cloud backups or
  device transfers.

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
