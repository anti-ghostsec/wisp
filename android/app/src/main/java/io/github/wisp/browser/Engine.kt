package io.github.wisp.browser

import android.content.Context
import android.util.Log
import org.json.JSONObject
import org.mozilla.geckoview.ContentBlocking
import org.mozilla.geckoview.GeckoPreferenceController
import org.mozilla.geckoview.GeckoPreferenceController.SetGeckoPreference
import org.mozilla.geckoview.GeckoResult
import org.mozilla.geckoview.GeckoRuntime
import org.mozilla.geckoview.GeckoRuntimeSettings
import org.mozilla.geckoview.GeckoWebExecutor
import org.mozilla.geckoview.WebExtension
import org.mozilla.geckoview.WebExtensionController
import org.mozilla.geckoview.WebRequest
import uniffi.wisp_ffi.Core
import uniffi.wisp_ffi.Host
import uniffi.wisp_ffi.PluginInfo
import java.io.File
import java.util.concurrent.Executors

/**
 * Wisp's engine on Android: one GeckoRuntime with Wisp's privacy settings,
 * the Rust core (wisp-core), and Wisp's plugins as built-in extensions.
 */
object Engine {
    private const val TAG = "wisp"
    /** The app's own settings, beside wisp-core's (config.json). */
    const val PREFS = "wisp-android"
    /**
     * Orbot's SOCKS port. Wisp can't tell Orbot from another app listening
     * there; the built-in Tor doesn't have that problem.
     */
    const val ORBOT_PORT = 9050
    /** Where Gecko is sent while the built-in Tor connects: nothing listens there. */
    private const val CLOSED_PORT = 9

    lateinit var runtime: GeckoRuntime
        private set
    lateinit var core: Core
        private set
    private lateinit var web: GeckoWebExecutor
    private val io = Executors.newCachedThreadPool()

    /** Each enabled plugin's extension, by plugin id. */
    val extensions = mutableMapOf<String, WebExtension>()
    var plugins: List<PluginInfo> = emptyList()
        private set

    /** wisp-core's settings (config.json), read when the engine starts. */
    lateinit var config: JSONObject
        private set

    val started get() = ::runtime.isInitialized

    fun start(context: Context) {
        if (started) return
        val app = context.applicationContext
        val pluginDir = unpackManifests(app)
        core = Core(
            File(app.filesDir, "config").path,
            File(app.filesDir, "data").path,
            File(app.cacheDir, "wisp").path,
            pluginDir.path,
            object : Host {
                override fun http(id: ULong, method: String, url: String, body: String?) = fetch(id, method, url, body)
            },
        )
        config = JSONObject(core.configJson())
        plugins = core.plugins()
        val dark = plugins.any { it.enabled && it.colorScheme == "dark" }
        val cfg = config
        // Trackers blocked: ads, analytics, social, fingerprinters, miners.
        // Not Gecko's "content" trackers, nor its strict list: those are
        // what a page embeds to work, among them Stripe (card payments) and
        // Cloudflare (sites' "are you human" checks), which then never load.
        // Those embeds get a cookie jar of their own for each site instead
        // (cookieBehavior), so they can't follow you from site to site; the
        // ad blocker's lists still take out the trackers among them.
        val blocking = ContentBlocking.Settings.Builder()
            .antiTracking(if (cfg.optBoolean("tracking_prevention", true)) ContentBlocking.AntiTracking.STRICT and ContentBlocking.AntiTracking.CONTENT.inv() else ContentBlocking.AntiTracking.DEFAULT)
            .enhancedTrackingProtectionLevel(ContentBlocking.EtpLevel.DEFAULT)
            .strictSocialTrackingProtection(true)
            .cookieBehavior(cookieBehavior(cfg))
            .cookieBehaviorPrivateMode(cookieBehavior(cfg))
            .cookiePurging(true)
            // Safe Browsing asks Google about the pages you visit.
            .safeBrowsing(ContentBlocking.SafeBrowsing.NONE)
            .queryParameterStrippingEnabled(cfg.optBoolean("strip_tracking_params", true))
            .queryParameterStrippingPrivateBrowsingEnabled(cfg.optBoolean("strip_tracking_params", true))
            .build()
        val settings = GeckoRuntimeSettings.Builder()
            .javaScriptEnabled(cfg.optBoolean("javascript", true) && level(app) != Level.SAFEST)
            .globalPrivacyControlEnabled(true)
            .loginAutofillEnabled(false)
            .remoteDebuggingEnabled(false)
            .aboutConfigEnabled(false)
            // Pages' console in logcat, for debug builds only.
            .consoleOutput(app.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0)
            .allowInsecureConnections(if (cfg.optBoolean("https_only", true)) GeckoRuntimeSettings.HTTPS_ONLY else GeckoRuntimeSettings.ALLOW_ALL)
            .preferredColorScheme(if (dark) GeckoRuntimeSettings.COLOR_SCHEME_DARK else GeckoRuntimeSettings.COLOR_SCHEME_SYSTEM)
            .contentBlocking(blocking)
            // Not as Android isolated processes: there, Android's video
            // decoders (MediaCodec) can't be reached, so H.264 video (most
            // streaming sites) has no decoder and the page's process crashes
            // (Mozilla bugs 1988727, 1988531; Firefox for Android doesn't
            // ship them either). Turn back on once Gecko's release builds
            // decode in its media process instead.
            .isolatedProcessEnabled(false)
            // Site isolation: each site in processes of its own, so a page
            // that takes over its process still can't read another site's.
            .fissionEnabled(true)
            .build()
        runtime = GeckoRuntime.create(app, settings)
        web = GeckoWebExecutor(runtime)
        // The route's settings first of all: Gecko keeps the last run's, and
        // on Tor that's a port the last Tor had, which any app may have
        // taken since. Until Tor is up again it's the closed port.
        firstPrefs = applyPrefs(app)
    }

    /** The first [applyPrefs], as the engine starts. */
    private var firstPrefs: GeckoResult<Map<String, Boolean>>? = null

    /**
     * Run [then] on the UI thread once the first settings are in force:
     * no extension or page loads before the route is set.
     */
    fun afterPrefs(then: () -> Unit) {
        val first = firstPrefs ?: return then()
        first.accept({ main.post { then() } }, { main.post { then() } })
    }

    /**
     * On: trackers get no cookies or storage at all, and anything else a
     * page embeds (a video player, say) gets a jar of its own for that one
     * site, so it can't follow you to other sites. Refusing embeds any
     * storage instead (as Wisp used to) broke most sites' video players,
     * which throw when they're refused it. Off: only trackers are refused.
     */
    fun cookieBehavior(cfg: JSONObject) =
        if (cfg.optBoolean("block_third_party_cookies", true)) ContentBlocking.CookieBehavior.ACCEPT_FIRST_PARTY_AND_ISOLATE_OTHERS
        else ContentBlocking.CookieBehavior.ACCEPT_NON_TRACKERS

    /** How pages reach the internet. */
    enum class Route { DIRECT, TOR, ORBOT }

    /** How much of the web Wisp turns off to make attacks harder. */
    enum class Level { STANDARD, SAFER, SAFEST }

    /** Wisp's settings, sealed with a key only this phone holds. */
    fun prefs(context: Context): android.content.SharedPreferences = SealedPrefs.get(context)

    fun route(context: Context): Route {
        val p = prefs(context)
        p.getString("route", null)?.let { name -> return Route.entries.find { it.name == name } ?: Route.DIRECT }
        // Before the built-in Tor: a tick meant Orbot.
        return if (p.getBoolean("tor", false)) Route.ORBOT else Route.DIRECT
    }

    fun setRoute(context: Context, route: Route) = prefs(context).edit().putString("route", route.name).remove("tor").apply()

    /** Safer unless chosen otherwise: no JIT compilers, which most browser exploits go through. */
    fun level(context: Context): Level =
        prefs(context).getString("security_level", null)?.let { name -> Level.entries.find { it.name == name } } ?: Level.SAFER

    fun setLevel(context: Context, level: Level) = prefs(context).edit().putString("security_level", level.name).apply()

    /** JavaScript runs at all: the setting, and not at the Safest level. */
    fun javascript(context: Context) = config.optBoolean("javascript", true) && level(context) != Level.SAFEST

    /**
     * Gecko settings GeckoView has no call for: what the security level
     * turns off, what desktop Wisp turns off in WebKit (WebGL, WebRTC),
     * Gecko's fingerprinting protection, and the route: straight out (your
     * VPN, if one is on), or through Tor (built in or Orbot's) with names
     * looked up there. On Tor, until it's connected, Gecko is pointed at a
     * port nothing listens on: pages fail, they never go around it. The
     * built-in Tor's port is the one it reports once it's up (it picks a
     * free one), never a set one another app could be listening on.
     *
     * Per-site circuits (Tor Browser gives each site its own SOCKS login,
     * and so its own circuit) need code GeckoView doesn't have, and there's
     * no pref for it: here all sites share Tor's circuits, rotated as Tor
     * does. Each site's cookies and storage are still kept apart.
     */
    fun applyPrefs(context: Context): GeckoResult<Map<String, Boolean>> {
        val cfg = config
        val route = route(context)
        val level = level(context)
        val tor = route != Route.DIRECT
        val standard = level == Level.STANDARD
        val noJit = !standard
        val webrtc = cfg.optBoolean("webrtc", false) && !tor && standard
        val webgl = cfg.optBoolean("webgl", false) && standard
        val socksPort = when (route) {
            Route.ORBOT -> ORBOT_PORT
            Route.TOR -> CLOSED_PORT
            Route.DIRECT -> ORBOT_PORT
        }
        // The built-in Tor listens on a Unix socket only Wisp can open; until
        // it's connected, Gecko is pointed at a closed port.
        val socksHost = if (route == Route.TOR && Tor.ready) "file://" + Tor.socket(context).absolutePath else "127.0.0.1"
        // Over Tor (or with fingerprinting resistance, which Tor turns on)
        // pages are asked for in US English, as every Tor Browser user's are.
        val languages = if (tor) "en-US, en" else systemLanguages(context)
        val user = GeckoPreferenceController.PREF_BRANCH_USER
        fun b(name: String, v: Boolean) = SetGeckoPreference.setBoolPref(name, v, user)
        fun i(name: String, v: Int) = SetGeckoPreference.setIntPref(name, v, user)
        fun s(name: String, v: String) = SetGeckoPreference.setStringPref(name, v, user)
        val prefs = listOf(
            b("webgl.disabled", !webgl),
            b("media.peerconnection.enabled", webrtc),
            b("media.navigator.enabled", webrtc),
            // Safer and Safest: scripts run without the JIT compilers (and so
            // without WebAssembly), which most browser exploits go through.
            b("javascript.options.ion", !noJit),
            b("javascript.options.baselinejit", !noJit),
            b("javascript.options.native_regexp", !noJit),
            b("javascript.options.wasm", !noJit),
            b("javascript.options.shared_memory", standard),
            // Rarely used, often attacked: off at every level.
            b("javascript.options.asmjs", false),
            b("dom.webgpu.enabled", false),
            b("gfx.font_rendering.graphite.enabled", false),
            b("gfx.font_rendering.opentype_svg.enabled", false),
            b("mathml.disabled", !standard),
            // Safest: only the fonts on the phone.
            b("gfx.downloadable_fonts.enabled", level != Level.SAFEST),
            // Protected (DRM) video through Android's Widevine: only for sites
            // you allow when asked (MainActivity's askDrm).
            b("media.eme.enabled", true),
            b("media.mediadrm-widevinecdm.visible", true),
            // Tabs are private (unless Keep cookies and site data is on):
            // there Gecko refuses pages service workers and their cache,
            // which many video players use and fail without. Kept in memory
            // only and gone when Wisp closes, as the rest of a private tab.
            b("dom.serviceWorkers.privateBrowsing.enabled", true),
            b("dom.cache.privateBrowsing.enabled", true),
            b("dom.indexedDB.privateBrowsing.enabled", true),
            b("privacy.fingerprintingProtection", true),
            b("privacy.fingerprintingProtection.pbmode", true),
            // On Tor, look like every other Tor user as far as Gecko can
            // (screen size, time zone, fonts, ...). Off the route, it breaks
            // too many sites for what it gives.
            b("privacy.resistFingerprinting", tor),
            b("privacy.resistFingerprinting.pbmode", tor),
            b("geo.enabled", false),
            b("dom.security.https_only_mode_send_http_background_request", false),
            b("network.prefetch-next", false),
            b("network.dns.disablePrefetch", true),
            b("network.predictor.enabled", false),
            b("browser.send_pings", false),
            // Gecko's own checks for a captive portal and for being online
            // ask Mozilla's servers every so often.
            b("network.captive-portal-service.enabled", false),
            b("network.connectivity-service.enabled", false),
            // Proxy: 1 = the SOCKS settings below, 5 = the system's.
            i("network.proxy.type", if (tor) 1 else 5),
            s("network.proxy.socks", socksHost),
            i("network.proxy.socks_port", socksPort),
            i("network.proxy.socks_version", 5),
            b("network.proxy.socks_remote_dns", true),
            // Its newer name: names are looked up through Tor, never here.
            b("network.proxy.socks5_remote_dns", true),
            // Tor not there: fail, never go around it.
            b("network.proxy.failover_direct", !tor),
            i("network.trr.mode", if (tor) 5 else 0),
            s("intl.accept_languages", languages),
            i("privacy.spoof_english", if (tor) 2 else 0),
            // A web page can't reach this phone's own servers (localhost) or
            // the local network: apps listening there can tie your visits to
            // your accounts in them. On Tor, localhost goes to Tor too, which
            // refuses it.
            b("network.lna.enabled", true),
            b("network.lna.blocking", true),
            b("network.proxy.allow_hijacking_localhost", tor),
            s("network.proxy.no_proxies_on", ""),
        )
        runtime.settings.setJavaScriptEnabled(javascript(context))
        return GeckoPreferenceController.setGeckoPrefs(prefs).map { result ->
            val failed = result?.filterValues { !it }?.keys.orEmpty()
            if (failed.isNotEmpty()) Log.w(TAG, "prefs not set: $failed")
            result
        }
    }

    /** The phone's languages, in its order, as Accept-Language lists them. */
    private fun systemLanguages(context: Context): String {
        val list = context.resources.configuration.locales
        val tags = (0 until list.size()).map { list[it].toLanguageTag() }.distinct()
        return tags.mapIndexed { n, t -> if (n == 0) t else "$t;q=${"%.1f".format(java.util.Locale.ROOT, maxOf(0.1, 1.0 - n * 0.1))}" }
            .joinToString(", ").ifEmpty { "en-US, en" }
    }

    /** What a Gecko pref is now, for checking (null if unset). */
    fun pref(name: String): GeckoResult<Any?> = GeckoPreferenceController.getGeckoPref(name).map { it?.value }

    /** The plugins' manifests, for wisp-core (it reads permissions and sync rules from them). */
    private fun unpackManifests(app: Context): File {
        val dir = File(app.filesDir, "plugins")
        dir.deleteRecursively()
        // "adblock+2" and the like are more of a plugin's blocking rules, no plugin of their own.
        for (id in app.assets.list("extensions").orEmpty().filter { '+' !in it }) {
            val target = File(dir, id).apply { mkdirs() }
            app.assets.open("extensions/$id/plugin.json").use { input ->
                File(target, "plugin.json").outputStream().use { input.copyTo(it) }
            }
        }
        return dir
    }

    /**
     * Install (or update) each plugin's extension and turn it on or off.
     * A plugin with more blocking rules than Gecko takes from one extension
     * (30,000) has the rest in extensions of their own, "<id>+2" and so on.
     */
    fun installExtensions(context: Context, onReady: () -> Unit) {
        // One at a time: GeckoView takes one built-in install at a time.
        val dirs = ArrayDeque(context.assets.list("extensions").orEmpty().toList())
        fun next() {
            val dir = dirs.removeFirstOrNull() ?: return onReady()
            installOne(dir) { next() }
        }
        next()
    }

    private fun installOne(dir: String, done: () -> Unit) {
        val controller = runtime.webExtensionController
        val id = dir.substringBefore('+')
        val part = dir != id
        val info = plugins.find { it.id == id }
        val extId = "${dir.replace('+', '-')}@plugins.wisp"
        controller.ensureBuiltIn("resource://android/assets/extensions/$dir/", extId)
            .then { ext ->
                if (ext == null || ext.metaData.allowedInPrivateBrowsing) GeckoResult.fromValue(ext)
                else controller.setAllowedInPrivateBrowsing(ext, true)
            }
            // Manifest V3 asks for access to sites separately: the
            // plugins' scripts run on every site, as on desktop.
            .then { ext ->
                if (ext == null || part || ext.metaData.grantedOptionalOrigins.contains("<all_urls>")) GeckoResult.fromValue(ext)
                else controller.addOptionalPermissions(extId, arrayOf(), arrayOf("<all_urls>"), arrayOf())
            }
            .then { ext ->
                when {
                    ext == null -> GeckoResult.fromValue(null)
                    info?.enabled == true && !ext.metaData.enabled -> controller.enable(ext, WebExtensionController.EnableSource.APP)
                    info?.enabled != true && ext.metaData.enabled -> controller.disable(ext, WebExtensionController.EnableSource.APP)
                    else -> GeckoResult.fromValue(ext)
                }
            }
            .accept({ ext ->
                if (!part) {
                    if (ext != null && info?.enabled == true) {
                        extensions[id] = ext
                        Plugins.attach(id, ext)
                    } else {
                        extensions.remove(id)
                    }
                }
                done()
            }, { e ->
                Log.e(TAG, "extension $dir: $e")
                done()
            })
    }

    /** Turn a plugin on or off, now and for later. */
    fun setPluginEnabled(context: Context, id: String, on: Boolean, done: () -> Unit) {
        core.setPluginEnabled(id, on)
        plugins = core.plugins()
        installExtensions(context, done)
    }

    fun reloadConfig() {
        config = JSONObject(core.configJson())
    }

    private val main = android.os.Handler(android.os.Looper.getMainLooper())

    /**
     * wisp-core's HTTP (sync), through Gecko's network: the same route as
     * pages, Orbot included. Called on the core's thread, it hands the
     * request to the UI thread (GeckoView's results need a thread with a
     * looper) and returns at once; the core always gets an answer, within
     * 30 seconds at the most.
     */
    /**
     * A GET through Gecko's network, so along the route (over Tor when on
     * Tor), with no cookies; redirects are followed. On the UI thread.
     */
    fun webGet(url: String): GeckoResult<org.mozilla.geckoview.WebResponse> {
        val request = WebRequest.Builder(url).apply {
            header("User-Agent", "wisp")
            cacheMode(WebRequest.CACHE_MODE_NO_STORE)
        }.build()
        return web.fetch(request, GeckoWebExecutor.FETCH_FLAGS_ANONYMOUS or GeckoWebExecutor.FETCH_FLAGS_PRIVATE)
    }

    /**
     * A picture from a page, for Save image and Share image: through Gecko's
     * network as the page got it (the route, the page's address as the
     * referrer, and the tab's cookies: private tabs' only). On the UI thread.
     */
    fun mediaGet(url: String, referrer: String, private: Boolean): GeckoResult<org.mozilla.geckoview.WebResponse> {
        val request = WebRequest.Builder(url).apply {
            if (referrer.startsWith("https://") || referrer.startsWith("http://")) referrer(referrer)
        }.build()
        return web.fetch(request, if (private) GeckoWebExecutor.FETCH_FLAGS_PRIVATE else 0)
    }

    private fun fetch(id: ULong, method: String, url: String, body: String?) {
        val answered = java.util.concurrent.atomic.AtomicBoolean(false)
        fun done(status: Int, bytes: ByteArray, error: String?) {
            if (answered.compareAndSet(false, true)) core.httpDone(id, status.toUShort(), bytes, error)
        }
        main.postDelayed({ done(0, ByteArray(0), "the sync service didn't answer in 30 seconds") }, 30_000)
        main.post {
            try {
                val request = WebRequest.Builder(url).method(method).apply {
                    header("User-Agent", "wisp")
                    if (body != null) {
                        header("Content-Type", "application/json")
                        body(body)
                    }
                    cacheMode(WebRequest.CACHE_MODE_NO_STORE)
                }.build()
                // No redirects followed: wisp-core refuses them (the sync
                // service must answer from its own address).
                val flags = GeckoWebExecutor.FETCH_FLAGS_ANONYMOUS or GeckoWebExecutor.FETCH_FLAGS_PRIVATE or
                    GeckoWebExecutor.FETCH_FLAGS_NO_REDIRECTS
                web.fetch(request, flags).accept({ response ->
                    if (response == null) return@accept done(0, ByteArray(0), "no response")
                    io.execute {
                        try {
                            response.setReadTimeoutMillis(20_000)
                            val bytes = response.body?.use { it.readBytes() } ?: ByteArray(0)
                            done(response.statusCode, bytes, null)
                        } catch (e: Exception) {
                            done(0, ByteArray(0), e.message ?: "read failed")
                        }
                    }
                }, { e -> done(0, ByteArray(0), e?.message ?: "request failed") })
            } catch (e: Exception) {
                done(0, ByteArray(0), e.message ?: "request failed")
            }
        }
    }
}
