# Writing Wisp plugins

A plugin is a folder with a `plugin.json` manifest and the files it references.
Install one with ☰ → **Install plugin…** (pick the plugin's folder), or by
copying its folder into `~/.local/share/wisp/plugins/`; then restart Wisp
and toggle it from the menu (☰). Wisp also loads plugins from `$WISP_PLUGIN_PATH`
(colon-separated), `/usr/share/wisp/plugins` and the `plugins/` folder of the
source tree.

## Manifest

```json
{
  "id": "my-plugin",
  "name": "My Plugin",
  "version": "1.0",
  "description": "Shown as a tooltip in the plugin menu.",
  "enabled_by_default": true,
  "permissions": ["storage", "tabs", "notifications", "sync"],
  "background": ["background.js"],
  "native": ["python3", "host/helper.py"],
  "sync_prefixes": ["x_"],
  "content_scripts": [
    {
      "matches": ["*://*/*"],
      "exclude_matches": ["https://bank.example/*"],
      "js": ["main.js"],
      "css": ["style.css"],
      "run_at": "document_end",
      "all_frames": false,
      "world": "isolated"
    }
  ],
  "blocklists": ["rules.json"],
  "filter_lists": [
    { "name": "EasyList", "url": "https://easylist.to/easylist/easylist.txt", "refresh_days": 4 }
  ],
  "actions": [
    { "id": "toggle", "label": "Do the thing", "icon": "icons/thing.svg", "shortcut": "<Ctrl>d" },
    { "id": "list", "label": "Open my page", "icon": "view-list-symbolic", "page": "list.html" },
    { "id": "menu", "label": "My popup", "icon": "icons/menu.svg", "popup": "popup.html" }
  ],
  "context_menu": [
    { "id": "insert", "label": "Insert the thing", "contexts": ["editable"] }
  ]
}
```

| Field | Meaning |
|---|---|
| `id` | Unique id. The first plugin found with a given id wins. |
| `enabled_by_default` | Used until the user toggles the plugin. Default `true`. |
| `permissions` | Which `wisp` API calls are allowed (see below). |
| `matches` / `exclude_matches` | WebKit URL patterns: `scheme://host/path`, `*` wildcards; `*://` = http and https. Default: all http(s) pages. |
| `run_at` | `document_start` (before page scripts) or `document_end` (DOM ready). |
| `all_frames` | Also run inside iframes. |
| `world` | `isolated` (default) or `page`; see below. |
| `blocklists` | [WebKit content-blocker](https://webkit.org/blog/3476/content-blockers-first-look/) JSON files. They are compiled to bytecode once and cached, which makes them far cheaper than JS-based blocking. |
| `filter_lists` | Adblock Plus / EasyList / uBlock lists, downloaded and converted to WebKit rules (network rules only; cosmetic `##` rules and options WebKit can't express are skipped). Refreshed every `refresh_days` days (default 4). |
| `actions` | Toolbar buttons (with optional shortcut). With `popup`, the button shows that plugin page in a popup under it, given `?tab=<id>` of the current tab; `window.close()` closes it. `icon` is either an `.svg` file in the plugin folder (single-color; its `<path>` shapes are drawn in the theme's text color, 16×16 viewBox) or an icon-theme name. With `page`, the action opens that plugin page in a new tab. Without it, an `action` event goes to the plugin's isolated script on the current page. |
| `context_menu` | Right-click menu entries. `contexts` says where they show: `editable` (text fields), `link`, `media` (video and audio), `selection` or `page` (everywhere, the default). Choosing one sends a `context-menu` event `{ id, tabId, pageUrl, pageTitle, linkUrl, mediaUrl }` to the plugin's background script if it has one, otherwise to its isolated script on that page. |
| `background` | Scripts that run in a hidden page (`wisp-plugin://<id>/_background.html`) for as long as the plugin is enabled, like an extension's background page. They have the `wisp` API. |
| `native` | A helper program the background script talks to (needs `native`): the command and its arguments. Arguments naming files in the plugin folder become full paths; the command is looked up on `PATH`. It speaks the WebExtensions native-messaging format (JSON preceded by a 32-bit little-endian length), so Firefox helpers work unchanged. It starts with the first message and stops when the plugin is disabled. |
| `color_scheme` | `"dark"`: while the plugin is enabled, pages are told the user prefers dark (`prefers-color-scheme: dark`) and Wisp's own UI turns dark. `dark-mode` uses it so sites show their own dark theme. |
| `sync_prefixes` | Title prefixes of synced entries this plugin may read and write. |

Plugin pages are files in the plugin folder, served at `wisp-plugin://<id>/<file>`
while the plugin is enabled. To run code on them, add a content script with
`"matches": ["wisp-plugin://<id>/*"]`.

Files can only be loaded from inside the plugin's own folder.

## Script worlds

- **`isolated`**: the script shares the page's DOM but has its own JavaScript
  globals, so the page can't see or tamper with it. Only isolated scripts get
  the `wisp` API.
- **`page`**: the script runs in the page's own JavaScript world, so it can
  override things the page sees (`navigator.*`, prototypes, …). It gets
  no API. `fingerprint-shield` is an example.

## The `wisp` API (isolated world)

Every call returns a Promise.

```js
wisp.plugin                      // { id, name, version }
await wisp.storage.get(key)      // "storage": JSON value or null
await wisp.storage.set(key, val) // persisted to ~/.local/share/wisp/plugin-data/, 1 MB quota
await wisp.storage.remove(key)
await wisp.storage.keys()
await wisp.tabs.open(url)        // "tabs": open an http(s) URL in a background tab, or one of
                                 // the plugin's own pages in front
await wisp.tabs.current()        // "tabs": { id, url, title } of the current tab
await wisp.runtime.sendMessage(msg)  // to the background script; resolves to its handler's answer
wisp.onMessage(async (msg) => answer) // background: answer messages (the first non-undefined answer wins)
await wisp.runtime.broadcast(msg)    // background: to the plugin's open pages ("broadcast" event)
await wisp.native.post(msg)      // "native": to the helper (started if needed); replies arrive as "native" events
await wisp.native.close()        // stop the helper ("native-disconnect" event when it's gone)
await wisp.action.setBadge(text) // up to 4 characters on the plugin's toolbar button(s); "" clears it
await wisp.downloads.start(url)  // "downloads": download an http(s) URL (the user picks where)
await wisp.cookies.netscape(url) // "cookies": that site's cookies as cookies.txt text
await wisp.openPath(path)        // "native": open a file or folder with the system's apps
await wisp.notify(title, body)   // "notifications": desktop notification
await wisp.log(...parts)         // prints to Wisp's stdout, no permission needed

await wisp.session.get/set/remove/keys(...)  // like storage, but memory only; cleared on exit

wisp.on("action", ({ id, url, title }) => { ... })  // a toolbar action was used on this page
wisp.on("context-menu", (info) => { ... })  // a context_menu entry was chosen (see the table);
                                            // in page scripts, listen for "contextmenu" events to know on what
wisp.on("media", ({ tabId, url, mime, size, pageUrl }) => { ... })  // background, "media": a page loaded video/audio
wisp.on("tab-updated", ({ tabId, url }) => { ... })  // background, "tabs": a tab went to a new page
wisp.on("tab-closed", ({ tabId }) => { ... })
wisp.browser.gpuCompositing      // true when pages are composited on the GPU

// "sync": encrypted xBrowserSync entries, limited to your sync_prefixes
await wisp.sync.list(prefix?)    // [{ id, title, name, url, description, tags }]; name = title minus prefix
await wisp.sync.add({ title, url, description?, tags? })   // returns the new id
await wisp.sync.update(id, { title?, url?, description?, tags? })
await wisp.sync.remove(id)
await wisp.sync.move(id, target, after = false)   // put entry `id` before (or after) entry `target`
await wisp.sync.status()         // { configured, service?, id?, pending? }
```

### Sync

Wisp includes an [xBrowserSync](https://www.xbrowsersync.org/) client. All
plugins share one account, and the data is encrypted on this device before
upload. Each plugin only sees entries whose titles start with one of its
`sync_prefixes`. The bundled plugins use `b_` for bookmarks and `p_` for
passwords. If you never set up an account, one is created automatically on
the official service the first time a plugin saves something. To use your own
account, or to sync another device, open ☰ → **Sync account…**.

Reads come from an encrypted local copy, refreshed from the service in the
background about once a minute. Writes apply to that copy straight away
(so `add` returns the new id at once) and are uploaded shortly after,
batched together; `status().pending` counts writes not uploaded yet. If
another device changed the data meanwhile, the writes are replayed on top of
its version.

Entries are ordinary xBrowserSync bookmarks, so the official xBrowserSync apps
show them too, including the `p_` entries. Those keep the login in their
description, readable by anyone who has your sync ID and password.

A call without the matching permission is rejected with an error. Each
plugin's message channel exists only in its own world, so web pages can't
reach it, and calls can't claim to come from a different plugin.

## Bundled examples

| Plugin | Shows how to |
|---|---|
| `adblock` | Use native blocklists and downloaded filter lists |
| `bookmarks` | Use toolbar actions, a plugin page and sync |
| `passwords` | Use sync, session storage, a context menu and isolated-world form filling |
| `fingerprint-shield` | Use a page-world script at `document_start` |
| `cookie-banner-hider` | Use CSS only |
| `dark-mode` | Use CSS and `color_scheme`, disabled by default |
| `vidgrab` | Use a background script, a native helper, a popup, badges, media events and context menus (a port of a Firefox add-on) |

## Debugging

Press Ctrl+Shift+I to open the Web Inspector. Plugin scripts show up as
`wisp-plugin://<id>/<file>`. `wisp.log()` output is printed to the terminal
that started Wisp.
