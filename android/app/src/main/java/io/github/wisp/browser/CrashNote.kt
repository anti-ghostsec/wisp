package io.github.wisp.browser

import android.app.Activity
import android.app.ActivityManager
import android.app.AlertDialog
import android.app.ApplicationExitInfo
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.widget.ScrollView
import android.widget.TextView
import java.io.File
import java.io.PrintWriter
import java.io.StringWriter
import java.text.DateFormat
import java.util.Date

/**
 * Why Wisp closed, last time it crashed: shown once at the next start, before
 * the browser engine is started, so a crash at start can still be read (and
 * copied, to report it). It stays in Wisp's own storage, nowhere else, and is
 * deleted as soon as it's shown. Wipe everything deletes it with the rest.
 */
object CrashNote {
    private const val FILE = "last-crash.txt"
    private const val SEEN = "crash_seen_at"

    /** From the first moment: an uncaught error is noted, then Android closes Wisp as before. */
    fun install(context: Context) {
        val app = context.applicationContext
        val before = Thread.getDefaultUncaughtExceptionHandler()
        if (before is Noter) return
        Thread.setDefaultUncaughtExceptionHandler(Noter(app, before))
    }

    private class Noter(val app: Context, val before: Thread.UncaughtExceptionHandler?) : Thread.UncaughtExceptionHandler {
        override fun uncaughtException(thread: Thread, error: Throwable) {
            try {
                val trace = StringWriter().also { error.printStackTrace(PrintWriter(it)) }.toString()
                val text = "Wisp ${version(app)}, Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT}), " +
                    "${Build.MANUFACTURER} ${Build.MODEL}\nThread: ${thread.name}\n\n$trace"
                File(app.filesDir, FILE).writeText(text.take(64 * 1024))
            } catch (_: Throwable) {
            }
            before?.uncaughtException(thread, error)
        }
    }

    /**
     * What there is to show, taken (deleted) as it's read: Wisp's own note, or
     * Android's record of a crash Wisp couldn't note itself (the engine's
     * native code), newer than the last one shown.
     */
    fun take(context: Context): String? {
        val file = File(context.filesDir, FILE)
        val noted = runCatching { file.readText() }.getOrNull()
        file.delete()
        // (Not Wisp's sealed settings: what a crash at start could be in.)
        val prefs = context.getSharedPreferences("crash", Context.MODE_PRIVATE)
        var android: String? = null
        if (Build.VERSION.SDK_INT >= 30) {
            val am = context.getSystemService(ActivityManager::class.java)
            val last = runCatching { am.getHistoricalProcessExitReasons(context.packageName, 0, 5) }.getOrNull().orEmpty()
                .firstOrNull { it.reason == ApplicationExitInfo.REASON_CRASH_NATIVE || it.reason == ApplicationExitInfo.REASON_CRASH }
            val seen = prefs.getLong(SEEN, -1L)
            if (last != null && last.timestamp > seen) {
                prefs.edit().putLong(SEEN, last.timestamp).apply()
                // Not one from long ago (the first time this is asked).
                if (noted == null && System.currentTimeMillis() - last.timestamp < 24 * 60 * 60_000L) {
                    val kind = if (last.reason == ApplicationExitInfo.REASON_CRASH_NATIVE) "the browser engine (native code)" else "Wisp"
                    android = "Wisp ${version(context)}, Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT}), " +
                        "${Build.MANUFACTURER} ${Build.MODEL}\n" +
                        "Crashed in $kind, ${DateFormat.getDateTimeInstance().format(Date(last.timestamp))}, " +
                        "process ${last.processName}\n${last.description ?: ""}"
                }
            }
        }
        return noted ?: android
    }

    /** The note, with Copy; then [then] (Wisp's usual start). */
    fun show(activity: Activity, text: String, then: () -> Unit) {
        val body = TextView(activity).apply {
            this.text = text
            setTextIsSelectable(true)
            textSize = 12f
            val pad = (16 * resources.displayMetrics.density).toInt()
            setPadding(pad, pad, pad, pad)
        }
        AlertDialog.Builder(activity)
            .setTitle("Wisp closed unexpectedly last time")
            .setView(ScrollView(activity).apply { addView(body) })
            .setPositiveButton("Copy") { _, _ ->
                activity.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("Wisp crash", text))
                then()
            }
            .setNegativeButton("Close") { _, _ -> then() }
            .setOnCancelListener { then() }
            .show()
    }

    private fun version(context: Context): String =
        runCatching { context.packageManager.getPackageInfo(context.packageName, 0).versionName }.getOrNull() ?: "?"
}
