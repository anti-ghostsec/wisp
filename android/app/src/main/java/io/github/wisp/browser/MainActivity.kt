package io.github.wisp.browser

import android.app.Activity
import android.app.AlertDialog
import android.content.ActivityNotFoundException
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.provider.MediaStore
import android.text.InputType
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.PopupMenu
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import org.json.JSONArray
import org.json.JSONObject
import org.mozilla.geckoview.AllowOrDeny
import org.mozilla.geckoview.ContentBlocking
import org.mozilla.geckoview.GeckoResult
import org.mozilla.geckoview.GeckoRuntimeSettings
import org.mozilla.geckoview.GeckoSession
import org.mozilla.geckoview.GeckoSession.ContentDelegate
import org.mozilla.geckoview.GeckoSession.NavigationDelegate
import org.mozilla.geckoview.GeckoSession.PermissionDelegate
import org.mozilla.geckoview.GeckoSession.ProgressDelegate
import org.mozilla.geckoview.GeckoView
import org.mozilla.geckoview.WebResponse
import uniffi.wisp_ffi.SavedTab
import java.io.File
import java.net.InetSocketAddress
import java.net.Socket
import java.text.DateFormat
import java.util.Date
import java.util.concurrent.Executors

class MainActivity : Activity(), Plugins.Ui {
    private lateinit var gecko: GeckoView
    private lateinit var address: EditText
    private lateinit var tabsButton: Button
    private lateinit var progress: ProgressBar
    private lateinit var notice: LinearLayout
    private lateinit var top: LinearLayout
    private val work = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private var ready = false
    private var fullScreen = false
    /** Links handed to Wisp before it was ready. */
    private val waiting = mutableListOf<String>()

    private val prefs get() = getSharedPreferences(Engine.PREFS, Context.MODE_PRIVATE)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        buildUi()
        Engine.start(this)
        Plugins.ui = this
        Tabs.delegates = { tab, session -> connect(tab, session) }
        intent?.takeIf { it.action == Intent.ACTION_VIEW }?.dataString?.let { waiting += it }
        askVault { Engine.installExtensions(this) { onReady() } }
    }

    private fun onReady() {
        if (ready) return
        ready = true
        work.execute { runCatching { Engine.core.syncWarmUp() } }
        if (Tabs.list.isEmpty()) {
            val saved = runCatching { Engine.core.loadTabs() }.getOrDefault(emptyList())
            saved.forEach { Tabs.list += Tab(it.url, it.title) }
        }
        waiting.forEach { openTab(it, true) }
        waiting.clear()
        if (Tabs.list.isEmpty()) openTab(home(), true) else show(Tabs.current ?: Tabs.list.last())
        if (Engine.torEnabled(this)) checkOrbot()
        startSuspendTimer()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        if (intent.action != Intent.ACTION_VIEW) return
        val url = intent.dataString ?: return
        if (ready) openTab(url, true) else waiting += url
    }

    override fun onPause() {
        super.onPause()
        if (!ready) return
        val tabs = Tabs.list.filter { isWebPage(it.url) }.map { SavedTab(it.url, it.title) }
        work.execute {
            runCatching { Engine.core.saveTabs(tabs) }
            runCatching { Engine.core.saveNow() }
        }
    }

    // ---- the window ----

    private fun dp(n: Int) = (n * resources.displayMetrics.density).toInt()

    private fun buildUi() {
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setBackgroundColor(Color.WHITE) }
        top = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(4), dp(4), dp(4), dp(4))
            setBackgroundColor(Color.rgb(0xc0, 0xc0, 0xc0))
        }
        address = EditText(this).apply {
            setSingleLine()
            hint = "Search or address"
            setTextColor(Color.BLACK)
            setBackgroundColor(Color.WHITE)
            setPadding(dp(8), dp(8), dp(8), dp(8))
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            imeOptions = EditorInfo.IME_ACTION_GO or EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
            setSelectAllOnFocus(true)
            setOnEditorActionListener { _, action, event ->
                if (action == EditorInfo.IME_ACTION_GO || event?.keyCode == KeyEvent.KEYCODE_ENTER) {
                    go(text.toString(), typed = true)
                    true
                } else false
            }
        }
        tabsButton = Button(this).apply { text = "1"; setOnClickListener { showTabs() } }
        val menu = Button(this).apply { text = "☰"; setOnClickListener { showMenu(it) } }
        bar.addView(address, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        bar.addView(tabsButton, LinearLayout.LayoutParams(dp(52), ViewGroup.LayoutParams.WRAP_CONTENT))
        bar.addView(menu, LinearLayout.LayoutParams(dp(52), ViewGroup.LayoutParams.WRAP_CONTENT))
        progress = ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply { max = 100; visibility = View.GONE }
        notice = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setBackgroundColor(Color.rgb(0xff, 0xff, 0xe1))
            setPadding(dp(8), dp(2), dp(4), dp(2))
            visibility = View.GONE
        }
        top.addView(bar)
        top.addView(progress, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(3)))
        top.addView(notice)
        gecko = GeckoView(this)
        root.addView(top)
        root.addView(gecko, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        // Android draws apps under its bars: keep clear of them and of the keyboard.
        root.setOnApplyWindowInsetsListener { v, insets ->
            if (Build.VERSION.SDK_INT >= 30) {
                val bars = insets.getInsets(android.view.WindowInsets.Type.systemBars() or android.view.WindowInsets.Type.ime())
                v.setPadding(bars.left, if (fullScreen) 0 else bars.top, bars.right, bars.bottom)
            } else {
                @Suppress("DEPRECATION")
                v.setPadding(insets.systemWindowInsetLeft, insets.systemWindowInsetTop, insets.systemWindowInsetRight, insets.systemWindowInsetBottom)
            }
            insets
        }
        setContentView(root)
    }

    /** A bar under the toolbar: a message and buttons; any button closes it. */
    private fun showNotice(text: String, vararg buttons: Pair<String, () -> Unit>) {
        notice.removeAllViews()
        notice.addView(TextView(this).apply { this.text = text; setTextColor(Color.BLACK) },
            LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        for ((label, run) in buttons.toList() + ("✕" to {})) {
            notice.addView(Button(this).apply {
                this.text = label
                setOnClickListener { notice.visibility = View.GONE; run() }
            })
        }
        notice.visibility = View.VISIBLE
    }

    private fun home(): String = Engine.config.optString("homepage").let { if (it.isEmpty() || it == "wisp:start") "about:blank" else it }

    // ---- tabs ----

    override fun context(): Context = this

    override fun openTab(url: String, focus: Boolean) {
        val tab = Tab(url)
        val at = Tabs.current?.let { Tabs.list.indexOf(it) + 1 } ?: Tabs.list.size
        Tabs.list.add(at.coerceIn(0, Tabs.list.size), tab)
        if (focus) show(tab) else updateTabCount()
    }

    override fun go(url: String) {
        val tab = Tabs.current ?: return openTab(url, true)
        tab.url = url
        tab.session.loadUri(url)
    }

    override fun currentTab(): JSONObject = Tabs.current?.json() ?: JSONObject()

    private fun show(tab: Tab) {
        val old = Tabs.current
        if (old != null && old != tab && old.loaded) old.session.setActive(false)
        Tabs.current = tab
        tab.lastShown = System.currentTimeMillis()
        val s = tab.session
        if (gecko.session != s) {
            gecko.releaseSession()
            gecko.setSession(s)
        }
        s.setActive(true)
        if (tab.url == "about:blank") {
            address.setText("")
            address.requestFocus()
        } else {
            address.clearFocus()
            address.setText(Plugins.displayUrl(tab.url))
        }
        progress.visibility = if (tab.progress < 100) View.VISIBLE else View.GONE
        updateTabCount()
    }

    private fun closeTab(tab: Tab) {
        val i = Tabs.list.indexOf(tab)
        Tabs.list.remove(tab)
        if (Tabs.current == tab) {
            Tabs.current = null
            gecko.releaseSession()
            if (Tabs.list.isEmpty()) openTab(home(), true) else show(Tabs.list[(i - 1).coerceIn(0, Tabs.list.size - 1)])
        }
        tab.close()
        updateTabCount()
    }

    private fun updateTabCount() {
        tabsButton.text = Tabs.list.size.toString()
    }

    private fun showTabs() {
        lateinit var dialog: AlertDialog
        val list = ListView(this)
        val adapter = object : ArrayAdapter<Tab>(this, 0, Tabs.list) {
            override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
                val tab = getItem(position)!!
                return LinearLayout(context).apply {
                    orientation = LinearLayout.HORIZONTAL
                    gravity = Gravity.CENTER_VERTICAL
                    setPadding(dp(12), dp(4), dp(4), dp(4))
                    val label = (if (tab == Tabs.current) "▸ " else "") + tab.title.ifEmpty { tab.url }
                    addView(TextView(context).apply { text = label; maxLines = 2; textSize = 16f },
                        LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                    addView(Button(context).apply {
                        text = "✕"
                        isFocusable = false
                        setOnClickListener { closeTab(tab); notifyDataSetChanged() }
                    })
                }
            }
        }
        list.adapter = adapter
        list.setOnItemClickListener { _, _, position, _ -> show(Tabs.list[position]); dialog.dismiss() }
        dialog = AlertDialog.Builder(this)
            .setTitle("Tabs")
            .setView(list)
            .setPositiveButton("New tab") { _, _ -> openTab(home(), true) }
            .setNegativeButton("Close", null)
            .show()
    }

    /** Background tabs unload after `suspend_tabs_after_minutes`; they keep their history. */
    private fun startSuspendTimer() {
        main.postDelayed(object : Runnable {
            override fun run() {
                val minutes = Engine.config.optInt("suspend_tabs_after_minutes", 15)
                if (minutes > 0) {
                    val before = System.currentTimeMillis() - minutes * 60_000L
                    Tabs.list.filter { it != Tabs.current && it.loaded && it.lastShown < before }.forEach { it.suspend() }
                }
                main.postDelayed(this, 60_000)
            }
        }, 60_000)
    }

    // ---- loading ----

    private fun go(text: String, typed: Boolean) {
        val t = text.trim()
        if (t.isEmpty()) return
        (getSystemService(INPUT_METHOD_SERVICE) as InputMethodManager).hideSoftInputFromWindow(address.windowToken, 0)
        address.clearFocus()
        val url = if (typed) Engine.core.resolveInput(t) else t
        when (url) {
            "wisp:history" -> return showHistory()
            "wisp:start" -> return go(home())
        }
        go(Plugins.pageUrl(url) ?: url)
    }

    private fun isWebPage(url: String) = url.startsWith("https://") || url.startsWith("http://")

    private fun connect(tab: Tab, session: GeckoSession) {
        session.navigationDelegate = object : NavigationDelegate {
            override fun onLocationChange(session: GeckoSession, url: String?, perms: MutableList<PermissionDelegate.ContentPermission>, hasUserGesture: Boolean) {
                tab.url = url ?: return
                if (tab == Tabs.current && !address.hasFocus()) address.setText(if (url == "about:blank") "" else Plugins.displayUrl(url))
                if (isWebPage(url)) work.execute { runCatching { Engine.core.recordVisit(url, tab.title) } }
            }

            override fun onCanGoBack(session: GeckoSession, canGoBack: Boolean) {
                tab.canGoBack = canGoBack
            }

            override fun onCanGoForward(session: GeckoSession, canGoForward: Boolean) {
                tab.canGoForward = canGoForward
            }

            override fun onLoadRequest(session: GeckoSession, request: NavigationDelegate.LoadRequest): GeckoResult<AllowOrDeny> {
                val uri = request.uri
                if (Engine.core.isAppLink(uri)) {
                    askAppLink(uri, tab.url)
                    return GeckoResult.fromValue(AllowOrDeny.DENY)
                }
                // Tracking parameters off before anything is sent.
                val clean = runCatching { Engine.core.cleanUrl(uri) }.getOrNull()
                if (clean != null && clean != uri) {
                    session.loadUri(clean)
                    return GeckoResult.fromValue(AllowOrDeny.DENY)
                }
                return GeckoResult.fromValue(AllowOrDeny.ALLOW)
            }

            override fun onSubframeLoadRequest(session: GeckoSession, request: NavigationDelegate.LoadRequest): GeckoResult<AllowOrDeny> =
                GeckoResult.fromValue(if (Engine.core.isAppLink(request.uri)) AllowOrDeny.DENY else AllowOrDeny.ALLOW)

            override fun onNewSession(session: GeckoSession, uri: String): GeckoResult<GeckoSession> {
                // Links with target=_blank, and pop-ups Gecko let through
                // (a click opened them, or you allowed them), open as tabs.
                val opened = Tab(uri)
                Tabs.list.add(Tabs.list.indexOf(tab) + 1, opened)
                val s = opened.forPopup()
                main.post { show(opened) }
                return GeckoResult.fromValue(s)
            }
        }
        session.progressDelegate = object : ProgressDelegate {
            override fun onPageStart(session: GeckoSession, url: String) {
                tab.progress = 0
                if (tab == Tabs.current) progress.visibility = View.VISIBLE
            }

            override fun onProgressChange(session: GeckoSession, p: Int) {
                tab.progress = p
                if (tab == Tabs.current) progress.progress = p
            }

            override fun onPageStop(session: GeckoSession, success: Boolean) {
                tab.progress = 100
                if (tab == Tabs.current) progress.visibility = View.GONE
            }

            override fun onSecurityChange(session: GeckoSession, info: ProgressDelegate.SecurityInformation) {
                tab.secure = info.isSecure
            }

            override fun onSessionStateChange(session: GeckoSession, state: GeckoSession.SessionState) {
                tab.state = state
            }
        }
        session.contentDelegate = object : ContentDelegate {
            override fun onTitleChange(session: GeckoSession, title: String?) {
                tab.title = title.orEmpty()
                val url = tab.url
                if (isWebPage(url) && !title.isNullOrEmpty()) work.execute { runCatching { Engine.core.setTitle(url, title) } }
            }

            override fun onCloseRequest(session: GeckoSession) = closeTab(tab)

            override fun onFullScreen(session: GeckoSession, fullScreen: Boolean) = setFullScreen(fullScreen)

            override fun onContextMenu(session: GeckoSession, screenX: Int, screenY: Int, element: ContentDelegate.ContextElement) =
                contextMenu(tab, element)

            override fun onExternalResponse(session: GeckoSession, response: WebResponse) = askDownload(response)

            override fun onCrash(session: GeckoSession) = tab.suspend()

            override fun onKill(session: GeckoSession) = tab.suspend()
        }
        session.permissionDelegate = object : PermissionDelegate {
            // Location, camera, microphone, notifications, ...: denied, as on desktop.
            override fun onContentPermissionRequest(session: GeckoSession, perm: PermissionDelegate.ContentPermission): GeckoResult<Int> =
                GeckoResult.fromValue(PermissionDelegate.ContentPermission.VALUE_DENY)

            override fun onAndroidPermissionsRequest(session: GeckoSession, permissions: Array<out String>?, callback: PermissionDelegate.Callback) =
                callback.reject()

            override fun onMediaPermissionRequest(
                session: GeckoSession,
                uri: String,
                video: Array<out PermissionDelegate.MediaSource>?,
                audio: Array<out PermissionDelegate.MediaSource>?,
                callback: PermissionDelegate.MediaCallback,
            ) = callback.reject()
        }
        session.promptDelegate = Prompts(this, tab)
    }

    /** A page's pop-up held by Gecko's blocker: shown in the bar, as on desktop. */
    fun popupBlocked(tab: Tab, target: String?, answer: (Boolean) -> Unit) {
        val site = Engine.core.siteOf(tab.url)
        val allowed = prefs.getStringSet("popups_allowed", emptySet())!!
        if (site.isNotEmpty() && site in allowed) return answer(true)
        val dest = target?.let { Uri.parse(it).host } ?: "a blank page"
        showNotice("Pop-up to $dest blocked",
            "Open" to { answer(true) },
            "Always allow on this site" to {
                prefs.edit().putStringSet("popups_allowed", allowed + site).apply()
                answer(true)
            })
    }

    private fun askAppLink(uri: String, page: String) {
        val scheme = uri.substringBefore(':').lowercase()
        val allowed = Engine.config.optJSONArray("allowed_app_links")
        val known = allowed?.let { a -> (0 until a.length()).map { a.optString(it) } }.orEmpty()
        val open = {
            try {
                startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(uri)).addCategory(Intent.CATEGORY_BROWSABLE))
            } catch (e: ActivityNotFoundException) {
                Toast.makeText(this, "No app opens $scheme: links", Toast.LENGTH_LONG).show()
            }
        }
        if (scheme in known) return open()
        val from = Engine.core.siteOf(page).ifEmpty { "This page" }
        showNotice("$from wants to open a $scheme: link in another app",
            "Open" to open,
            "Always open" to {
                runCatching { Engine.core.setConfig(JSONObject().put("allowed_app_links", JSONArray(known + scheme)).toString()) }
                Engine.reloadConfig()
                open()
            })
    }

    private fun setFullScreen(on: Boolean) {
        fullScreen = on
        top.visibility = if (on) View.GONE else View.VISIBLE
        if (on) window.addFlags(WindowManager.LayoutParams.FLAG_FULLSCREEN) else window.clearFlags(WindowManager.LayoutParams.FLAG_FULLSCREEN)
    }

    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        val tab = Tabs.current
        when {
            fullScreen && tab != null -> tab.session.exitFullScreen()
            notice.visibility == View.VISIBLE -> notice.visibility = View.GONE
            tab != null && tab.canGoBack -> tab.session.goBack()
            else -> @Suppress("DEPRECATION") super.onBackPressed()
        }
    }

    // ---- long-press menu ----

    private fun contextMenu(tab: Tab, el: ContentDelegate.ContextElement) {
        val items = mutableListOf<Pair<String, () -> Unit>>()
        val link = el.linkUri
        val media = el.srcUri?.takeIf { el.type != ContentDelegate.ContextElement.TYPE_NONE }
        if (link != null) {
            items += "Open in new tab" to { openTab(link, false) }
            items += "Copy link" to { copy(link) }
        }
        if (media != null) items += "Open media in new tab" to { openTab(media, false) }
        val contexts = buildSet {
            add("page")
            if (link != null) add("link")
            if (media != null) add("media")
        }
        for (p in Engine.plugins.filter { it.enabled && Engine.extensions.containsKey(it.id) }) {
            for (item in p.contextMenu.filter { m -> m.contexts.any { it in contexts } }) {
                items += item.label to {
                    val data = JSONObject().put("id", item.id).put("tabId", tab.id).put("pageUrl", tab.url).put("pageTitle", tab.title)
                        .put("linkUrl", link ?: JSONObject.NULL).put("mediaUrl", media ?: JSONObject.NULL)
                    Plugins.emit(p.id, tab.session, "context-menu", data)
                }
            }
        }
        if (items.isEmpty()) return
        AlertDialog.Builder(this)
            .setTitle(link ?: media ?: tab.title)
            .setItems(items.map { it.first }.toTypedArray()) { _, i -> items[i].second() }
            .show()
    }

    private fun copy(text: String) {
        val clipboard = getSystemService(android.content.ClipboardManager::class.java)
        clipboard.setPrimaryClip(android.content.ClipData.newPlainText("link", text))
    }

    // ---- the ☰ menu ----

    private fun showMenu(anchor: View) {
        val popup = PopupMenu(this, anchor)
        val actions = mutableMapOf<Int, () -> Unit>()
        var next = 1
        fun add(label: String, run: () -> Unit) {
            actions[next] = run
            popup.menu.add(0, next++, 0, label)
        }
        val tab = Tabs.current
        if (tab?.canGoForward == true) add("Forward") { tab.session.goForward() }
        add("Reload") { tab?.session?.reload() }
        add("New tab") { openTab(home(), true) }
        for (p in Engine.plugins.filter { it.enabled && Engine.extensions.containsKey(it.id) }) {
            for (a in p.actions) add(a.label) { runAction(p.id, a.id, a.page) }
        }
        add("History") { showHistory() }
        add("Settings") { showSettings() }
        add("Plugins") { showPlugins() }
        add("Wisp password") { changePassword() }
        add("Close all tabs") {
            Tabs.list.toList().forEach { it.close() }
            Tabs.list.clear()
            Tabs.current = null
            gecko.releaseSession()
            openTab(home(), true)
        }
        popup.setOnMenuItemClickListener { actions[it.itemId]?.invoke(); true }
        popup.show()
    }

    private fun runAction(plugin: String, action: String, page: String?) {
        val tab = Tabs.current
        if (page != null) {
            val base = Engine.extensions[plugin]?.metaData?.baseUrl ?: return
            openTab(base + page + (tab?.let { "?tab=${it.id}" } ?: ""), true)
            return
        }
        val data = JSONObject().put("id", action).put("url", tab?.url ?: JSONObject.NULL).put("title", tab?.title ?: JSONObject.NULL)
        Plugins.emit(plugin, tab?.session, "action", data)
    }

    private fun showHistory() {
        work.execute {
            val visits = runCatching { Engine.core.history() }.getOrDefault(emptyList()).take(500)
            main.post {
                if (visits.isEmpty()) {
                    val why = if (Engine.config.optBoolean("save_history", false)) "No history yet" else "History is off: turn on Save history in Settings"
                    Toast.makeText(this, why, Toast.LENGTH_LONG).show()
                    return@post
                }
                val fmt = DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT)
                val labels = visits.map { "${it.title.ifEmpty { it.url }}\n${fmt.format(Date(it.last * 1000))}" }.toTypedArray()
                AlertDialog.Builder(this)
                    .setTitle("History")
                    .setItems(labels) { _, i -> openTab(visits[i].url, true) }
                    .setNeutralButton("Clear history") { _, _ -> work.execute { runCatching { Engine.core.clearHistory() } } }
                    .setNegativeButton("Close", null)
                    .show()
            }
        }
    }

    // ---- settings ----

    private class Setting(val label: String, val key: String, val default: Boolean, val restart: Boolean = false)

    private val settings = listOf(
        Setting("HTTPS only", "https_only", true),
        Setting("Remove tracking parameters", "strip_tracking_params", true),
        Setting("Block third-party cookies", "block_third_party_cookies", true),
        Setting("Strict tracking protection", "tracking_prevention", true, restart = true),
        Setting("JavaScript", "javascript", true),
        Setting("Safer JavaScript (slower)", "no_jit", false),
        Setting("WebGL", "webgl", false),
        Setting("WebRTC", "webrtc", false),
        Setting("Save history", "save_history", false),
        Setting("Reopen tabs", "restore_tabs", false),
        Setting("Keep cookies and site data", "persistent_profile", false, restart = true),
    )

    private fun showSettings() {
        val cfg = Engine.config
        val labels = (settings.map { it.label } + "Route through Orbot (Tor)").toTypedArray()
        val checked = (settings.map { cfg.optBoolean(it.key, it.default) } + Engine.torEnabled(this)).toBooleanArray()
        val before = checked.copyOf()
        AlertDialog.Builder(this)
            .setTitle("Settings")
            .setMultiChoiceItems(labels, checked) { _, i, on -> checked[i] = on }
            .setPositiveButton("Save") { _, _ ->
                val changes = JSONObject()
                settings.forEachIndexed { i, s -> if (checked[i] != before[i]) changes.put(s.key, checked[i]) }
                val tor = checked.last()
                val restart = settings.indices.any { checked[it] != before[it] && settings[it].restart }
                try {
                    if (changes.length() > 0) Engine.core.setConfig(changes.toString())
                } catch (e: Exception) {
                    Toast.makeText(this, e.message, Toast.LENGTH_LONG).show()
                }
                prefs.edit().putBoolean("tor", tor).apply()
                Engine.reloadConfig()
                applyLive()
                if (tor && !before.last()) checkOrbot()
                if (restart) Toast.makeText(this, "Some changes take effect when Wisp next starts", Toast.LENGTH_LONG).show()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /** What GeckoView can change while it runs; pages pick it up as they reload. */
    private fun applyLive() {
        val cfg = Engine.config
        val s = Engine.runtime.settings
        s.setAllowInsecureConnections(if (cfg.optBoolean("https_only", true)) GeckoRuntimeSettings.HTTPS_ONLY else GeckoRuntimeSettings.ALLOW_ALL)
        s.setJavaScriptEnabled(cfg.optBoolean("javascript", true))
        val cb = s.contentBlocking
        val behavior = if (cfg.optBoolean("block_third_party_cookies", true)) ContentBlocking.CookieBehavior.ACCEPT_FIRST_PARTY
            else ContentBlocking.CookieBehavior.ACCEPT_FIRST_PARTY_AND_ISOLATE_OTHERS
        cb.setCookieBehavior(behavior)
        cb.setCookieBehaviorPrivateMode(behavior)
        cb.setQueryParameterStrippingEnabled(cfg.optBoolean("strip_tracking_params", true))
        cb.setQueryParameterStrippingPrivateBrowsingEnabled(cfg.optBoolean("strip_tracking_params", true))
        Engine.applyPrefs(this)
    }

    /** Orbot listens on 127.0.0.1:9050 while it runs; without it pages don't load (never around it). */
    private fun checkOrbot() {
        work.execute {
            val up = runCatching { Socket().use { it.connect(InetSocketAddress("127.0.0.1", Engine.ORBOT_PORT), 1500) }; true }.getOrDefault(false)
            if (up) return@execute
            main.post {
                showNotice("Orbot isn't running: pages won't load until it is",
                    "Start Orbot" to {
                        try {
                            sendBroadcast(Intent("org.torproject.android.intent.action.START").setPackage("org.torproject.android"))
                            startActivity(packageManager.getLaunchIntentForPackage("org.torproject.android")!!)
                        } catch (e: Exception) {
                            Toast.makeText(this, "Orbot isn't installed", Toast.LENGTH_LONG).show()
                        }
                    })
            }
        }
    }

    private fun showPlugins() {
        val list = Engine.plugins
        val labels = list.map { it.name }.toTypedArray()
        val checked = list.map { it.enabled }.toBooleanArray()
        AlertDialog.Builder(this)
            .setTitle("Plugins")
            .setMultiChoiceItems(labels, checked) { _, i, on -> checked[i] = on }
            .setPositiveButton("Save") { _, _ ->
                val changed = list.indices.filter { checked[it] != list[it].enabled }
                if (changed.isEmpty()) return@setPositiveButton
                for (i in changed.drop(1)) Engine.core.setPluginEnabled(list[i].id, checked[i])
                Engine.setPluginEnabled(this, list[changed[0]].id, checked[changed[0]]) {
                    Toast.makeText(this, "Reload pages to apply", Toast.LENGTH_SHORT).show()
                }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    // ---- downloads ----

    private fun fileName(response: WebResponse): String {
        val disposition = response.headers["Content-Disposition"] ?: response.headers["content-disposition"]
        val fromHeader = disposition?.let { Regex("filename\\*?=(?:UTF-8'')?\"?([^\";]+)").find(it)?.groupValues?.get(1) }
        val raw = fromHeader ?: Uri.parse(response.uri).lastPathSegment ?: "download"
        return Uri.decode(raw).replace(Regex("[/\\\\:*?\"<>|\\x00-\\x1f]"), "_").trim('.', ' ').ifEmpty { "download" }.take(120)
    }

    private fun askDownload(response: WebResponse) {
        val name = fileName(response)
        val size = response.headers["Content-Length"]?.toLongOrNull()?.let { " (${android.text.format.Formatter.formatShortFileSize(this, it)})" } ?: ""
        AlertDialog.Builder(this)
            .setTitle("Download")
            .setMessage("Save $name$size to Downloads?\n\nFrom ${Uri.parse(response.uri).host}")
            .setPositiveButton("Save") { _, _ -> save(response, name) }
            .setNegativeButton("Cancel") { _, _ -> work.execute { runCatching { response.body?.close() } } }
            .show()
    }

    private fun save(response: WebResponse, name: String) {
        val mime = response.headers["Content-Type"]?.substringBefore(';') ?: "application/octet-stream"
        work.execute {
            val result = runCatching {
                val input = response.body ?: error("nothing to save")
                if (Build.VERSION.SDK_INT >= 29) {
                    val values = ContentValues().apply {
                        put(MediaStore.Downloads.DISPLAY_NAME, name)
                        put(MediaStore.Downloads.MIME_TYPE, mime)
                        put(MediaStore.Downloads.IS_PENDING, 1)
                    }
                    val uri = contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: error("can't create the file")
                    contentResolver.openOutputStream(uri)!!.use { out -> input.use { it.copyTo(out) } }
                    values.clear()
                    values.put(MediaStore.Downloads.IS_PENDING, 0)
                    contentResolver.update(uri, values, null, null)
                } else {
                    val dir = getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: filesDir
                    File(dir, name).outputStream().use { out -> input.use { it.copyTo(out) } }
                }
            }
            main.post {
                Toast.makeText(this, result.fold({ "Saved $name" }, { "Download failed: ${it.message}" }), Toast.LENGTH_LONG).show()
            }
        }
    }

    // ---- the vault ----

    private fun passwordField(hint: String) = EditText(this).apply {
        this.hint = hint
        inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
        imeOptions = EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
    }

    private fun form(vararg views: View) = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(dp(20), dp(8), dp(20), 0)
        views.forEach { addView(it) }
    }

    /** Slow vault work (Argon2id) off the UI thread, then `done` with its error, if any. */
    private fun vaultWork(what: String, f: () -> Unit, done: (String?) -> Unit) {
        val wait = AlertDialog.Builder(this).setMessage(what).setCancelable(false).show()
        work.execute {
            val error = runCatching(f).exceptionOrNull()?.let { it.message ?: it.toString() }
            main.post { wait.dismiss(); done(error) }
        }
    }

    /**
     * Before the first tab: the vault (saved logins, bookmarks, history and
     * tabs) is set up or unlocked, as on desktop. Put off, Wisp runs as if
     * nothing were kept.
     */
    private fun askVault(then: () -> Unit) {
        val status = Engine.core.vaultStatus()
        when {
            status.downgraded -> AlertDialog.Builder(this)
                .setTitle("Your Wisp password is gone")
                .setMessage("Wisp's vault had a password and now has none, but Wisp didn't remove it: something else replaced the file. Use it without a password only if you did that yourself.")
                .setPositiveButton("Use it anyway") { _, _ -> Engine.core.vaultAcceptDowngrade(); then() }
                .setNegativeButton("Not now") { _, _ -> then() }
                .setCancelable(false)
                .show()
            !status.exists -> setupVault(then)
            !status.unlocked -> unlockVault(then)
            else -> then()
        }
    }

    private fun setupVault(then: () -> Unit) {
        val one = passwordField("Password")
        val two = passwordField("Password again")
        AlertDialog.Builder(this)
            .setTitle("Choose a Wisp password")
            .setMessage("It locks your saved logins, bookmarks, history and tabs on this phone. Without one they're still kept, sealed with a key stored beside them.")
            .setView(form(one, two))
            .setCancelable(false)
            .setPositiveButton("Use password") { _, _ ->
                val pw = one.text.toString()
                when {
                    pw.isEmpty() -> { Toast.makeText(this, "Type a password, or choose No password", Toast.LENGTH_LONG).show(); setupVault(then) }
                    pw != two.text.toString() -> { Toast.makeText(this, "The passwords differ", Toast.LENGTH_LONG).show(); setupVault(then) }
                    else -> vaultWork("Setting up…", { Engine.core.vaultCreate(pw) }) { e -> if (e != null) Toast.makeText(this, e, Toast.LENGTH_LONG).show(); then() }
                }
            }
            .setNegativeButton("No password") { _, _ -> vaultWork("Setting up…", { Engine.core.vaultCreate(null) }) { _ -> then() } }
            .show()
    }

    private fun unlockVault(then: () -> Unit) {
        val field = passwordField("Wisp password")
        AlertDialog.Builder(this)
            .setTitle("Unlock Wisp")
            .setView(form(field))
            .setCancelable(false)
            .setPositiveButton("Unlock") { _, _ ->
                vaultWork("Unlocking…", { Engine.core.vaultUnlock(field.text.toString()) }) { e ->
                    if (e != null) {
                        Toast.makeText(this, e, Toast.LENGTH_LONG).show()
                        unlockVault(then)
                    } else then()
                }
            }
            .setNegativeButton("Not now") { _, _ -> then() }
            .show()
    }

    private fun changePassword() {
        val status = Engine.core.vaultStatus()
        if (!status.unlocked) return unlockVault {}
        val one = passwordField("New password")
        val two = passwordField("New password again")
        AlertDialog.Builder(this)
            .setTitle("Wisp password")
            .setView(form(one, two))
            .setPositiveButton("Change") { _, _ ->
                val pw = one.text.toString()
                if (pw.isEmpty() || pw != two.text.toString()) {
                    Toast.makeText(this, if (pw.isEmpty()) "Type a password" else "The passwords differ", Toast.LENGTH_LONG).show()
                } else {
                    vaultWork("Changing…", { Engine.core.vaultSetPassword(pw) }) { e -> Toast.makeText(this, e ?: "Password changed", Toast.LENGTH_LONG).show() }
                }
            }
            .apply {
                if (status.hasPassword) setNeutralButton("Remove password") { _, _ ->
                    vaultWork("Changing…", { Engine.core.vaultSetPassword(null) }) { e -> Toast.makeText(this@MainActivity, e ?: "Password removed", Toast.LENGTH_LONG).show() }
                }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }
}
