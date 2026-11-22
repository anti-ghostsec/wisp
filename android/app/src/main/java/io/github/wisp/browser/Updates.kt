package io.github.wisp.browser

import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import androidx.core.content.FileProvider
import org.json.JSONObject
import java.io.File
import java.security.MessageDigest
import java.util.concurrent.Executors

/**
 * Updates from Wisp's GitHub releases, fetched the way pages are (over Tor
 * when that's the route, which also gets past a block on GitHub): checked
 * once a week while on the built-in Tor, or when you ask (☰ → More →
 * Check for updates). An update is installed only if its SHA-256 matches
 * the release's, and its signing certificate is this Wisp's own; Android
 * asks before installing it.
 */
object Updates {
    private const val LATEST = "https://api.github.com/repos/anti-ghostsec/wisp/releases/latest"
    private const val WEEK = 7 * 86_400_000L
    private val io = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())

    class Release(val tag: String, val version: Long, val apk: String, val sha256: String)

    private fun apkName() = if (Build.SUPPORTED_ABIS.firstOrNull() == "x86_64") "wisp-android-x86_64.apk" else "wisp-android-arm64.apk"

    private fun ownVersion(context: Context): Long {
        val info = context.packageManager.getPackageInfo(context.packageName, 0)
        @Suppress("DEPRECATION")
        return if (Build.VERSION.SDK_INT >= 28) info.longVersionCode else info.versionCode.toLong()
    }

    /** A weekly check is due: only on the built-in Tor, once it's connected. */
    fun due(context: Context) = Engine.route(context) == Engine.Route.TOR && Tor.ready &&
        System.currentTimeMillis() - Engine.prefs(context).getLong("update_checked", 0) > WEEK

    /** The newest release if it's newer than this Wisp (null if not); on the UI thread. */
    fun check(context: Context, done: (Release?, String?) -> Unit) {
        val app = context.applicationContext
        Engine.prefs(app).edit().putLong("update_checked", System.currentTimeMillis()).apply()
        text(LATEST) { body, error ->
            if (body == null) return@text done(null, error)
            try {
                val json = JSONObject(body)
                val assets = json.getJSONArray("assets")
                fun url(name: String): String? = (0 until assets.length()).map { assets.getJSONObject(it) }
                    .firstOrNull { it.optString("name") == name }?.optString("browser_download_url")?.takeIf { it.startsWith("https://") }
                val name = apkName()
                val apk = url(name)
                val sha = url("$name.sha256")
                val version = url("$name.version")
                if (apk == null || sha == null || version == null) return@text done(null, "the newest release has no $name yet")
                val tag = json.optString("tag_name")
                text(version) { v, e ->
                    val n = v?.trim()?.toLongOrNull() ?: return@text done(null, e ?: "the release's version isn't readable")
                    done(if (n > ownVersion(app)) Release(tag, n, apk, sha) else null, null)
                }
            } catch (e: Exception) {
                done(null, e.message ?: e.toString())
            }
        }
    }

    /** Download `release`, check it, and hand it to Android's installer. */
    fun install(context: Context, release: Release, progress: (String) -> Unit, done: (String?) -> Unit) {
        val app = context.applicationContext
        text(release.sha256) { shaText, shaError ->
            val want = shaText?.trim()?.substringBefore(' ')?.lowercase()?.takeIf { it.length == 64 }
                ?: return@text done(shaError ?: "the release's checksum isn't readable")
            Engine.webGet(release.apk).accept({ response ->
                if (response == null || response.statusCode != 200) return@accept done("the download failed (${response?.statusCode})")
                io.execute {
                    val file = File(File(app.cacheDir, "update").apply { mkdirs() }, "wisp.apk")
                    val failure = runCatching {
                        response.setReadTimeoutMillis(120_000)
                        val digest = MessageDigest.getInstance("SHA-256")
                        val input = response.body ?: error("nothing came")
                        var total = 0L
                        var shown = 0L
                        input.use { inp ->
                            file.outputStream().use { out ->
                                val buf = ByteArray(64 * 1024)
                                while (true) {
                                    val n = inp.read(buf)
                                    if (n < 0) break
                                    out.write(buf, 0, n)
                                    digest.update(buf, 0, n)
                                    total += n
                                    if (total - shown > 4_000_000) {
                                        shown = total
                                        val mb = total / 1_000_000
                                        main.post { progress("Downloading the update… $mb MB") }
                                    }
                                }
                            }
                        }
                        val got = digest.digest().joinToString("") { "%02x".format(it) }
                        if (got != want) error("the download doesn't match the release's checksum")
                        verify(app, file, release)
                    }.exceptionOrNull()
                    if (failure != null) file.delete()
                    main.post {
                        if (failure != null) done(failure.message ?: failure.toString()) else done(open(app, file))
                    }
                }
            }, { e -> done(e?.message ?: "the download failed") })
        }
    }

    /** Wisp itself, newer, signed with this Wisp's own key. */
    private fun verify(context: Context, file: File, release: Release) {
        val pm = context.packageManager
        @Suppress("DEPRECATION")
        val flags = if (Build.VERSION.SDK_INT >= 28) PackageManager.GET_SIGNING_CERTIFICATES else PackageManager.GET_SIGNATURES
        val archive = pm.getPackageArchiveInfo(file.path, flags) ?: error("the download isn't an app")
        if (archive.packageName != context.packageName) error("the download is another app")
        @Suppress("DEPRECATION")
        val version = if (Build.VERSION.SDK_INT >= 28) archive.longVersionCode else archive.versionCode.toLong()
        if (version != release.version || version <= ownVersion(context)) error("the download isn't the newer Wisp")
        val theirs = certs(archive)
        val ours = certs(pm.getPackageInfo(context.packageName, flags))
        // Android refuses an update signed with another key in any case; this
        // says so before it gets that far. Where Android doesn't tell an
        // archive's signer (some versions), its own check is what's left.
        if (theirs.isNotEmpty() && theirs != ours) error("the download isn't signed with Wisp's key")
    }

    private fun certs(info: PackageInfo): Set<String> {
        @Suppress("DEPRECATION")
        val sigs = if (Build.VERSION.SDK_INT >= 28) info.signingInfo?.apkContentsSigners else info.signatures
        return sigs.orEmpty().map { s -> MessageDigest.getInstance("SHA-256").digest(s.toByteArray()).joinToString("") { "%02x".format(it) } }.toSet()
    }

    /** Android's installer for `file`; null when it's showing, or what's wrong. */
    private fun open(context: Context, file: File): String? {
        if (Build.VERSION.SDK_INT >= 26 && !context.packageManager.canRequestPackageInstalls()) {
            context.startActivity(
                Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}"))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
            return "Allow Wisp to install apps (its updates), then check for updates again"
        }
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
        @Suppress("DEPRECATION")
        val intent = Intent(Intent.ACTION_INSTALL_PACKAGE).setDataAndType(uri, "application/vnd.android.package-archive")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        return try {
            context.startActivity(intent)
            null
        } catch (e: Exception) {
            e.message ?: e.toString()
        }
    }

    /** A small text (up to 1 MB) from `url`, through Gecko; the answer comes on the UI thread. */
    private fun text(url: String, done: (String?, String?) -> Unit) {
        Engine.webGet(url).accept({ response ->
            if (response == null || response.statusCode != 200) return@accept done(null, "the release server answered ${response?.statusCode}")
            io.execute {
                val result = runCatching {
                    response.setReadTimeoutMillis(60_000)
                    val bytes = response.body?.use { input ->
                        val out = java.io.ByteArrayOutputStream()
                        val buf = ByteArray(16 * 1024)
                        while (true) {
                            val n = input.read(buf)
                            if (n < 0) break
                            out.write(buf, 0, n)
                            if (out.size() > 1_000_000) error("too big")
                        }
                        out.toByteArray()
                    } ?: ByteArray(0)
                    String(bytes, Charsets.UTF_8)
                }
                main.post { done(result.getOrNull(), result.exceptionOrNull()?.message) }
            }
        }, { e -> done(null, e?.message ?: "the release server didn't answer") })
    }
}
