package io.github.wisp.browser

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import androidx.core.content.FileProvider
import org.mozilla.geckoview.WebResponse
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors

/**
 * Downloads: saved in Wisp's own storage, which no other app can read and
 * which Wipe deletes. A file goes elsewhere only when you open, share or
 * save it to the phone ("Save to phone…", wherever you pick). What's
 * downloading now (and what failed) is kept in memory; what's finished is
 * the folder itself. While a download runs a notification shows its
 * progress, never the file's name; it opens the list.
 */
object Downloads {
    private const val CHANNEL = "downloads"

    enum class State { RUNNING, FAILED, CANCELLED }

    /** A download started this run: running, failed or cancelled. */
    class Job(val id: Int, val name: String, val mime: String, val total: Long) {
        @Volatile var done = 0L
        @Volatile var state = State.RUNNING
        @Volatile var error: String? = null
        @Volatile var cancel = false
    }

    /** A finished download: a file in Wisp's downloads folder. */
    class File(val name: String, val mime: String, val size: Long, val modified: Long, val uri: Uri, val path: java.io.File)

    val jobs = CopyOnWriteArrayList<Job>()
    private val listeners = CopyOnWriteArrayList<() -> Unit>()
    private val io = Executors.newCachedThreadPool()
    private val main = Handler(Looper.getMainLooper())
    private var nextId = 1000

    fun listen(f: () -> Unit) = listeners.add(f)
    fun unlisten(f: () -> Unit) = listeners.remove(f)
    private fun changed() = main.post { listeners.forEach { it() } }

    private fun dir(context: Context) = java.io.File(context.applicationContext.filesDir, "downloads")
    private fun partDir(context: Context) = java.io.File(context.applicationContext.filesDir, "downloads.part")

    /** Save what `response` brings as `name`, in the background. */
    fun start(context: Context, response: WebResponse, name: String) {
        val app = context.applicationContext
        val mime = response.headers["Content-Type"]?.substringBefore(';')?.trim()?.ifEmpty { null } ?: "application/octet-stream"
        val total = response.headers["Content-Length"]?.toLongOrNull() ?: -1L
        val job = Job(nextId++, safeName(name), mime, total)
        jobs.add(0, job)
        changed()
        io.execute { run(app, job, response) }
    }

    /** A plain file name: no folders, nothing hidden. */
    private fun safeName(name: String) =
        name.substringAfterLast('/').substringAfterLast('\\').trim().trimStart('.').take(120).ifEmpty { "download" }

    /** `name`, or "name (2).ext" and so on if that's taken. */
    private fun freeName(dir: java.io.File, name: String): java.io.File {
        var f = java.io.File(dir, name)
        var n = 2
        val stem = name.substringBeforeLast('.', name)
        val ext = name.substringAfterLast('.', "").let { if (it.isEmpty() || it == name) "" else ".$it" }
        while (f.exists()) f = java.io.File(dir, "$stem (${n++})$ext")
        return f
    }

    private fun run(app: Context, job: Job, response: WebResponse) {
        // Written beside the folder and moved in when complete, so the list
        // never shows half a file.
        val part = java.io.File(partDir(app).apply { mkdirs() }, "${job.id}")
        val result = runCatching {
            val input = response.body ?: error("the site sent nothing to save")
            input.use {
                part.outputStream().use { o ->
                    val buf = ByteArray(64 * 1024)
                    var last = 0L
                    while (true) {
                        if (job.cancel) error("cancelled")
                        val n = it.read(buf)
                        if (n < 0) break
                        o.write(buf, 0, n)
                        job.done += n
                        val now = System.currentTimeMillis()
                        if (now - last > 500) {
                            last = now
                            notifyProgress(app, job)
                            changed()
                        }
                    }
                }
            }
            synchronized(this) {
                val target = freeName(dir(app).apply { mkdirs() }, job.name)
                if (!part.renameTo(target)) error("can't save the file")
            }
        }
        val manager = app.getSystemService(NotificationManager::class.java)
        result.onSuccess {
            jobs.remove(job)
            notifyDone(app, job)
        }.onFailure { e ->
            // Nothing half-saved is left behind.
            part.delete()
            if (job.cancel) {
                job.state = State.CANCELLED
                jobs.remove(job)
                manager.cancel(job.id)
            } else {
                job.state = State.FAILED
                job.error = e.message ?: e.toString()
                notifyFailed(app, job)
            }
        }
        changed()
    }

    /** The finished downloads, newest first. */
    fun files(context: Context): List<File> =
        dir(context).listFiles().orEmpty().filter { it.isFile }.sortedByDescending { it.lastModified() }.map {
            File(it.name, mimeOf(it.name), it.length(), it.lastModified(),
                FileProvider.getUriForFile(context, "${context.packageName}.files", it), it)
        }

    /** Copy `file` to where you picked (a document Android's file picker made). */
    fun export(context: Context, file: File, to: Uri, done: (String?) -> Unit) {
        val app = context.applicationContext
        io.execute {
            val error = runCatching {
                (app.contentResolver.openOutputStream(to) ?: error("can't write there")).use { o -> file.path.inputStream().use { it.copyTo(o) } }
            }.exceptionOrNull()?.let { it.message ?: it.toString() }
            main.post { done(error) }
        }
    }

    fun delete(context: Context, file: File) {
        file.path.delete()
        changed()
    }

    fun forget(job: Job) {
        jobs.remove(job)
        changed()
    }

    private fun mimeOf(name: String) =
        android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase())
            ?: "application/octet-stream"

    // ---- the notification ----

    private fun channel(context: Context): NotificationManager {
        val manager = context.getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(CHANNEL) == null) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL, "Downloads", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "Files Wisp is downloading"
                    // On a locked screen: that something downloads, not what.
                    lockscreenVisibility = Notification.VISIBILITY_PRIVATE
                },
            )
        }
        return manager
    }

    private fun openList(context: Context) = PendingIntent.getActivity(
        context, 0, Intent(context, DownloadsActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    private fun builder(context: Context, title: String, text: String) = Notification.Builder(context, CHANNEL)
        .setSmallIcon(android.R.drawable.stat_sys_download)
        .setContentTitle(title)
        .setContentText(text)
        .setContentIntent(openList(context))
        .setVisibility(Notification.VISIBILITY_PRIVATE)
        .setPublicVersion(
            Notification.Builder(context, CHANNEL).setSmallIcon(android.R.drawable.stat_sys_download)
                .setContentTitle("Wisp").setContentText("Downloads").build(),
        )

    private fun notifyProgress(context: Context, job: Job) {
        val n = builder(context, "Downloading", progressText(context, job)).setOngoing(true).setOnlyAlertOnce(true).apply {
            if (job.total > 0) setProgress(100, (job.done * 100 / job.total).toInt(), false) else setProgress(0, 0, true)
        }.build()
        runCatching { channel(context).notify(job.id, n) }
    }

    private fun notifyDone(context: Context, job: Job) {
        val n = builder(context, "Download finished", "In Wisp's downloads")
            .setSmallIcon(android.R.drawable.stat_sys_download_done).setAutoCancel(true).build()
        runCatching { channel(context).notify(job.id, n) }
    }

    private fun notifyFailed(context: Context, job: Job) {
        val n = builder(context, "Download failed", "Open Downloads to see why")
            .setSmallIcon(android.R.drawable.stat_notify_error).setAutoCancel(true).build()
        runCatching { channel(context).notify(job.id, n) }
    }

    fun progressText(context: Context, job: Job): String {
        fun size(b: Long) = android.text.format.Formatter.formatShortFileSize(context, b)
        return if (job.total > 0) "${size(job.done)} of ${size(job.total)} · ${job.done * 100 / job.total}%" else size(job.done)
    }
}
