package io.github.wisp.browser

import IPtProxy.Controller
import IPtProxy.IPtProxy
import IPtProxy.OnTransportEvents
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.ServiceConnection
import android.os.IBinder
import android.util.Log
import androidx.localbroadcastmanager.content.LocalBroadcastManager
import org.json.JSONObject
import org.torproject.jni.TorService
import java.io.File
import java.util.concurrent.Executors

/**
 * Tor, built into Wisp (the Guardian Project's tor-android: the tor Orbot
 * runs). It runs inside Wisp while the route is Tor; Gecko is pointed at it
 * once it's connected, and at a closed port until then, so nothing ever
 * goes around it.
 *
 * Its SOCKS listener is a Unix socket in Wisp's own folder, not a port on
 * 127.0.0.1: any app on the phone can reach a local port, only Wisp can
 * open its own files.
 *
 * With bridges, tor reaches the network through a pluggable transport
 * (obfs4, Snowflake, meek or webtunnel, from IPtProxy, as in Orbot), so
 * a network that blocks Tor doesn't see Tor.
 */
object Tor {
    private const val TAG = "wisp"
    const val BRIDGES_FAILED = "BRIDGES_FAILED"

    /** The bridges tor uses: none, Tor Project's built-in ones, or your own lines. */
    enum class Bridges { NONE, OBFS4, SNOWFLAKE, MEEK, CUSTOM }

    /** Whether tor is connected and Gecko can use [socket]. */
    var ready = false
        private set
    var status = TorService.STATUS_OFF
        private set

    @Volatile private var bound = false
    private var onChange: ((String) -> Unit)? = null
    private var transports: Controller? = null
    private val work = Executors.newSingleThreadExecutor()
    private val main = android.os.Handler(android.os.Looper.getMainLooper())

    /** Tor's SOCKS socket (in a folder only Wisp can open). */
    fun socket(context: Context) = File(socketDir(context), "socks")

    private fun socketDir(context: Context) = File(context.applicationContext.filesDir, "tor-socks")

    fun bridges(context: Context): Bridges = runCatching {
        Bridges.valueOf(Engine.prefs(context).getString("bridges", null) ?: "NONE")
    }.getOrDefault(Bridges.NONE)

    fun customBridges(context: Context): String = Engine.prefs(context).getString("bridge_lines", "") ?: ""

    fun setBridges(context: Context, kind: Bridges, custom: String) {
        Engine.prefs(context).edit().putString("bridges", kind.name).putString("bridge_lines", custom.trim()).apply()
    }

    /** The bridge lines for [kind]: the built-in ones bundled with Wisp, or yours. */
    fun lines(context: Context, kind: Bridges = bridges(context)): List<String> {
        if (kind == Bridges.NONE) return emptyList()
        if (kind == Bridges.CUSTOM) return parse(customBridges(context))
        val all = JSONObject(context.assets.open("bridges.json").bufferedReader().use { it.readText() })
        val list = all.optJSONArray(kind.name.lowercase()) ?: return emptyList()
        return (0 until list.length()).map { list.getString(it) }
    }

    private val TRANSPORTS = setOf(IPtProxy.Obfs4, IPtProxy.Snowflake, IPtProxy.MeekLite, IPtProxy.Webtunnel)

    /**
     * Bridge lines as people paste them: one per line, with or without the
     * leading "Bridge", blank lines and # comments ignored. Only lines for a
     * transport Wisp has (or plain relay addresses) are kept.
     */
    fun parse(text: String): List<String> = text.lines()
        .map { it.trim().removePrefix("Bridge ").removePrefix("bridge ").trim() }
        .filter { it.isNotEmpty() && !it.startsWith("#") && !it.any { c -> c.isISOControl() } }
        .filter { line ->
            val first = line.substringBefore(' ')
            first in TRANSPORTS || first.first().isDigit() || first.startsWith("[")
        }

    private val receiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            status = intent.getStringExtra(TorService.EXTRA_STATUS) ?: return
            ready = status == TorService.STATUS_ON && socket(context).exists()
            // Read by now: the bridge lines in it don't stay on disk in the clear.
            if (status == TorService.STATUS_ON) TorService.getTorrc(context).delete()
            // Pages reload (onChange) only once Gecko is pointed at Tor.
            val changed = status
            Engine.applyPrefs(context).accept({ onChange?.invoke(changed) }, { onChange?.invoke(changed) })
        }
    }

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {}

        override fun onServiceDisconnected(name: ComponentName) {
            ready = false
            status = TorService.STATUS_OFF
        }
    }

    val running get() = bound

    fun start(context: Context, onChange: (String) -> Unit) {
        this.onChange = onChange
        if (bound) return
        bound = true
        val app = context.applicationContext
        LocalBroadcastManager.getInstance(app).registerReceiver(receiver, IntentFilter(TorService.ACTION_STATUS))
        work.execute {
            // Tor's guards and directory, from their sealed copies.
            runCatching { unsealFiles(app) }.onFailure { Log.w(TAG, "tor: files: $it") }
            val torrc = try {
                config(app)
            } catch (e: Exception) {
                Log.e(TAG, "tor: bridges: $e")
                null
            }
            main.post {
                if (!bound) return@post
                if (torrc == null) {
                    // Without its bridges tor isn't started: plain Tor is
                    // what a network that blocks it looks for.
                    status = BRIDGES_FAILED
                    onChange(status)
                    return@post
                }
                TorService.getTorrc(app).writeText(torrc)
                if (!app.bindService(Intent(app, TorService::class.java), connection, Context.BIND_AUTO_CREATE)) {
                    status = TorService.STATUS_OFF
                    onChange(status)
                }
            }
        }
    }

    /** The torrc: a private SOCKS socket, and the bridges with their transports started. */
    private fun config(app: Context): String {
        val dir = socketDir(app)
        dir.mkdirs()
        // 0700: tor refuses a socket in a folder others can reach, and no other app may.
        dir.setReadable(false, false); dir.setWritable(false, false); dir.setExecutable(false, false)
        dir.setReadable(true, true); dir.setWritable(true, true); dir.setExecutable(true, true)
        socket(app).delete()
        val out = StringBuilder()
        // Names are looked up through Tor (SOCKS requests that carry only an
        // address are refused), and nothing it can avoid goes to disk.
        out.append("SocksPort unix:\"${socket(app).absolutePath}\"\n")
        // tor-android's defaults also open an HTTP tunnel port, which any app
        // could use as much as a SOCKS port: none.
        out.append("HTTPTunnelPort 0\n")
        out.append("SafeSocks 1\nAvoidDiskWrites 1\n")
        val lines = lines(app)
        if (lines.isEmpty()) return out.toString()
        val used = lines.map { it.substringBefore(' ') }.filter { it in TRANSPORTS }.toSet()
        val pt = transports ?: IPtProxy.newController(File(app.filesDir, "pt").apply { mkdirs() }.absolutePath, false, false, "ERROR", object : OnTransportEvents {
            override fun connected(name: String) {}
            override fun error(name: String, e: Exception) { Log.w(TAG, "tor: $name: $e") }
            override fun stopped(name: String, e: Exception?) {}
        }).also { transports = it }
        lines.firstOrNull { it.startsWith(IPtProxy.Snowflake + " ") }?.let { snowflake(pt, it) }
        out.append("UseBridges 1\n")
        for (t in used) {
            pt.start(t, "")
            val port = pt.port(t)
            if (port <= 0) throw IllegalStateException("$t didn't start")
            out.append("ClientTransportPlugin $t socks5 127.0.0.1:$port\n")
        }
        for (line in lines) out.append("Bridge ").append(line).append('\n')
        return out.toString()
    }

    /** Snowflake takes its broker, fronts and STUN servers from the controller, not from tor. */
    private fun snowflake(pt: Controller, line: String) {
        val args = line.split(' ').mapNotNull { part -> part.split('=', limit = 2).takeIf { it.size == 2 }?.let { it[0] to it[1] } }.toMap()
        args["url"]?.let { pt.snowflakeBrokerUrl = it }
        (args["fronts"] ?: args["front"])?.let { pt.snowflakeFrontDomains = it }
        args["ice"]?.let { pt.snowflakeIceServers = it }
        args["ampcache"]?.let { pt.snowflakeAmpCacheUrl = it }
        args["sqsqueue"]?.let { pt.snowflakeSqsUrl = it }
        args["sqscreds"]?.let { pt.snowflakeSqsCreds = it }
    }

    fun stop(context: Context) {
        if (!bound) return
        val app = context.applicationContext
        runCatching { app.unbindService(connection) }
        LocalBroadcastManager.getInstance(app).unregisterReceiver(receiver)
        bound = false
        ready = false
        status = TorService.STATUS_OFF
        transports?.let { pt -> work.execute { TRANSPORTS.forEach { runCatching { pt.stop(it) } } } }
        Engine.applyPrefs(app)
        // Once tor has shut down (and written its state), its files are sealed.
        main.postDelayed({ if (!bound) work.execute { sealFiles(app) } }, 8_000)
    }

    private fun dataDir(app: Context) = File(app.getDir("TorService", Context.MODE_PRIVATE), "data")
    private fun sealedDir(app: Context) = File(app.filesDir, "tor-sealed")

    /**
     * Tor's own files (its guards, in "state", and the directory it
     * downloaded) sealed ([Sealed]) and the plain ones deleted, along with
     * the torrc and the transports' state: while Tor isn't running, nothing
     * on the phone says how Wisp reaches it. Only while tor isn't running;
     * also at start-up, for what a Wisp that was killed left in the clear.
     */
    fun sealIdle(context: Context) {
        val app = context.applicationContext
        if (!bound) work.execute { sealFiles(app) }
    }

    private fun sealFiles(context: Context) {
        val app = context.applicationContext
        if (bound) return
        val data = dataDir(app)
        val files = data.walkTopDown().filter { it.isFile && it.name != "lock" }.toList()
        if (files.isNotEmpty()) {
            val out = sealedDir(app)
            out.deleteRecursively()
            out.mkdirs()
            for (f in files) {
                runCatching { Sealed.write(File(out, f.relativeTo(data).path.replace('/', '%')), f.readBytes()) }
                    .onFailure { Log.w(TAG, "tor: can't seal ${f.name}: $it") }
            }
        }
        data.deleteRecursively()
        TorService.getTorrc(app).delete()
        File(app.filesDir, "pt").deleteRecursively()
        // Its folder is gone: the next start makes a new one.
        transports = null
        socket(app).delete()
    }

    /** Tor's files back where it reads them (if a sealed copy is all there is). */
    private fun unsealFiles(app: Context) {
        val from = sealedDir(app)
        val data = dataDir(app)
        if (!from.isDirectory || data.walkTopDown().any { it.isFile && it.name == "state" }) return
        for (f in from.listFiles().orEmpty()) {
            if (f.name.endsWith(".new")) continue
            val plain = Sealed.read(f) ?: continue
            val target = File(data, f.name.replace('%', '/'))
            target.parentFile?.mkdirs()
            target.writeBytes(plain)
        }
    }
}
