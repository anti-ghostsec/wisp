package io.github.wisp.browser

import org.json.JSONObject
import org.mozilla.geckoview.GeckoSession
import org.mozilla.geckoview.GeckoSessionSettings

/**
 * A tab: its session (created when the tab is first shown, as on desktop
 * Wisp, where background tabs wait), its address and title, and what's
 * kept while it's unloaded (suspended after a while in the background).
 */
class Tab(var url: String, var title: String = "") {
    val id = nextId++
    private var _session: GeckoSession? = null
    /** Back/forward history and form state, kept when the tab unloads. */
    var state: GeckoSession.SessionState? = null
    var progress = 100
    var canGoBack = false
    var canGoForward = false
    var secure = false
    var lastShown = System.currentTimeMillis()

    val loaded get() = _session != null
    val session: GeckoSession get() = _session ?: open()

    /** Created, opened and given its first address (or its kept state). */
    private fun open(): GeckoSession {
        val s = create()
        s.open(Engine.runtime)
        Plugins.attachAll(s)
        val kept = state
        if (kept != null) s.restoreState(kept) else s.loadUri(url)
        return s
    }

    /** A session for a page's new window: GeckoView opens and loads it itself. */
    fun forPopup(): GeckoSession {
        val s = create()
        Plugins.attachAll(s)
        return s
    }

    private fun create(): GeckoSession {
        val settings = GeckoSessionSettings.Builder()
            .usePrivateMode(!Engine.config.optBoolean("persistent_profile", false))
            .useTrackingProtection(true)
            // Playing tabs keep playing in the background, as on desktop.
            .suspendMediaWhenInactive(false)
            .apply { Engine.config.optString("user_agent").takeIf { it.isNotEmpty() && it != "null" }?.let { userAgentOverride(it) } }
            .build()
        val s = GeckoSession(settings)
        _session = s
        Tabs.delegates?.invoke(this, s)
        return s
    }

    /** Unload: the page goes, its history stays. */
    fun suspend() {
        val s = _session ?: return
        _session = null
        s.close()
    }

    fun close() {
        _session?.close()
        _session = null
    }

    fun json() = JSONObject().put("id", id).put("url", url).put("title", title)

    companion object {
        private var nextId = 1L
    }
}

object Tabs {
    val list = mutableListOf<Tab>()
    var current: Tab? = null
    /** Sets a new session's delegates (the activity's). */
    var delegates: ((Tab, GeckoSession) -> Unit)? = null

    fun all() = list.filter { it.loaded }

    fun of(session: GeckoSession) = list.find { it.loaded && it.session == session }
}
