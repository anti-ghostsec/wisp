package io.github.wisp.browser

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PersistableBundle
import android.util.Log
import android.widget.Toast
import org.json.JSONObject
import org.json.JSONTokener
import org.mozilla.geckoview.GeckoResult
import org.mozilla.geckoview.GeckoSession
import org.mozilla.geckoview.WebExtension
import java.util.concurrent.Executors

/**
 * The `wisp` API for the plugins' extensions: calls arrive by native
 * messaging ("wisp"), wisp-core answers the ones that keep data (storage,
 * session values, synced bookmarks and logins), the app the rest. Which
 * page asked comes from GeckoView (the sender's address and kind), never
 * from what the page says, so a web page gets only its own site's logins.
 */
object Plugins {
    private const val TAG = "wisp"
    // A thread per call in flight: one waiting on the sync service never holds up the others.
    private val work = Executors.newCachedThreadPool()
    private val main = Handler(Looper.getMainLooper())

    /** What plugins can ask of the browser around them. */
    interface Ui {
        fun openTab(url: String, focus: Boolean)
        fun go(url: String)
        fun currentTab(): JSONObject
        fun context(): Context
    }

    var ui: Ui? = null

    /** Event ports: plugin id to the ports its pages opened, with their tab. */
    private val ports = mutableMapOf<String, MutableList<WebExtension.Port>>()

    /** The extension of `id` is on: answer its pages and its scripts in every tab. */
    fun attach(id: String, ext: WebExtension) {
        ext.setMessageDelegate(delegate(id, ext), "wisp")
        Tabs.all().forEach { attachTo(it.session, id, ext) }
    }

    fun attachTo(session: GeckoSession, id: String, ext: WebExtension) {
        session.webExtensionController.setMessageDelegate(ext, delegate(id, ext), "wisp")
    }

    /** Every enabled plugin's scripts in a new tab. */
    fun attachAll(session: GeckoSession) {
        Engine.extensions.forEach { (id, ext) -> attachTo(session, id, ext) }
    }

    /** `wisp-plugin://<id>/<page>` (desktop addresses) to the extension's page. */
    fun pageUrl(url: String): String? {
        val rest = url.removePrefix("wisp-plugin://").takeIf { it != url } ?: return null
        val id = rest.substringBefore('/')
        val base = Engine.extensions[id]?.metaData?.baseUrl ?: return null
        return base + rest.substringAfter('/', "")
    }

    fun isPluginPage(url: String) = Engine.extensions.values.any { url.startsWith(it.metaData.baseUrl) }

    /** A plugin page's address as desktop Wisp writes it: `wisp-plugin://<id>/<page>`. */
    fun displayUrl(url: String): String {
        for ((id, ext) in Engine.extensions) {
            val base = ext.metaData.baseUrl
            if (url.startsWith(base)) return "wisp-plugin://$id/" + url.removePrefix(base)
        }
        return url
    }

    /** Send event `type` to plugin `id`'s scripts in `session` (or all its pages). */
    fun emit(id: String, session: GeckoSession?, type: String, data: JSONObject) {
        val msg = JSONObject().put("type", type).put("data", data)
        ports[id].orEmpty().filter { session == null || it.sender.session == session }.forEach {
            try {
                it.postMessage(msg)
            } catch (e: Exception) {
                Log.w(TAG, "plugin $id: $e")
            }
        }
    }

    private fun delegate(id: String, ext: WebExtension) = object : WebExtension.MessageDelegate {
        override fun onMessage(nativeApp: String, message: Any, sender: WebExtension.MessageSender): GeckoResult<Any>? {
            val msg = message as? JSONObject ?: return GeckoResult.fromValue(error("bad message").toString())
            val op = msg.optString("op")
            val args = msg.optJSONObject("args") ?: JSONObject()
            val ownPage = sender.environmentType == WebExtension.MessageSender.ENV_TYPE_EXTENSION &&
                sender.url.startsWith(ext.metaData.baseUrl)
            val result = GeckoResult<Any>()
            work.execute {
                val reply = try {
                    val value = Engine.core.pluginCall(id, op, args.toString(), sender.url, ownPage)
                    if (value != null) answer(JSONTokener(value).nextValue()) else hostCall(id, op, args, sender)
                } catch (e: Exception) {
                    error(e.message ?: e.toString())
                }
                // As JSON text: GeckoView passes strings back to the page, not JSON objects.
                main.post { result.complete(reply.toString()) }
            }
            return result
        }

        override fun onConnect(port: WebExtension.Port) {
            ports.getOrPut(id) { mutableListOf() }.add(port)
            port.setDelegate(object : WebExtension.PortDelegate {
                override fun onDisconnect(port: WebExtension.Port) {
                    ports[id]?.remove(port)
                }
            })
        }
    }

    private fun error(text: String) = JSONObject().put("error", text)

    /** A call's result; null is left out (GeckoView can't pass it), and the API reads it as null. */
    private fun answer(value: Any?) = JSONObject().apply { if (value != null && value != JSONObject.NULL) put("value", value) }

    private fun need(id: String, permission: String) {
        if (!Engine.core.pluginHasPermission(id, permission)) {
            throw IllegalStateException("plugin '$id' lacks the \"$permission\" permission")
        }
    }

    private fun isWeb(url: String) = url.startsWith("https://") || url.startsWith("http://")

    /** On the UI thread, waiting for it (we're on the plugin worker). */
    private fun <T> onMain(f: () -> T): T {
        val result = GeckoResult<T>()
        main.post {
            try {
                result.complete(f())
            } catch (e: Exception) {
                result.completeExceptionally(e)
            }
        }
        return result.poll(10_000)!!
    }

    /** The calls the app answers: tabs, the clipboard, notices. */
    private fun hostCall(id: String, op: String, args: JSONObject, sender: WebExtension.MessageSender): JSONObject {
        val ui = ui ?: return error("Wisp isn't ready")
        val ok = answer(null)
        when (op) {
            "tabs.open" -> {
                need(id, "tabs")
                val url = args.optString("url")
                val own = pageUrl(url) ?: url.takeIf { Engine.extensions[id]?.metaData?.baseUrl?.let(url::startsWith) == true }
                if (!isWeb(url) && own == null) return error("tabs.open only accepts http(s) URLs and the plugin's own pages")
                onMain { ui.openTab(own ?: url, own != null) }
            }
            "tabs.go" -> {
                need(id, "tabs")
                val url = args.optString("url")
                if (!isWeb(url)) return error("tabs.go only accepts http(s) URLs")
                onMain { ui.go(url) }
            }
            "tabs.current" -> {
                need(id, "tabs")
                return answer(onMain { ui.currentTab() })
            }
            "clipboard.write" -> {
                need(id, "clipboard")
                val text = args.optString("text")
                val secret = args.optBoolean("secret")
                onMain { copy(ui.context(), text, secret) }
            }
            "notify" -> {
                need(id, "notifications")
                val text = listOf(args.optString("title"), args.optString("body")).filter { it.isNotEmpty() }.joinToString(": ")
                onMain { Toast.makeText(ui.context(), text, Toast.LENGTH_LONG).show() }
            }
            "action.badge" -> {}
            else -> return error("\"$op\" isn't available in Wisp for Android")
        }
        return ok
    }

    /**
     * Copy text; a secret (a password) is marked sensitive, so the
     * keyboard's clipboard history and the copy preview leave it out, and
     * cleared after 45 seconds if it's still there.
     */
    private fun copy(context: Context, text: String, secret: Boolean) {
        val clipboard = context.getSystemService(ClipboardManager::class.java)
        val clip = ClipData.newPlainText("Wisp", text)
        if (secret) {
            clip.description.extras = PersistableBundle().apply {
                if (Build.VERSION.SDK_INT >= 33) putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true)
                else putBoolean("android.content.extra.IS_SENSITIVE", true)
            }
        }
        clipboard.setPrimaryClip(clip)
        if (secret) {
            main.postDelayed({
                val now = clipboard.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.text?.toString()
                if (now == text) {
                    if (Build.VERSION.SDK_INT >= 28) clipboard.clearPrimaryClip()
                    else clipboard.setPrimaryClip(ClipData.newPlainText("", ""))
                }
            }, 45_000)
        }
    }
}
