package io.github.wisp.browser

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.ContentUris
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.provider.MediaStore
import androidx.core.content.FileProvider
import org.mozilla.geckoview.WebResponse
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors

/**
 * Downloads: saved to Download/Wisp, where file managers and other apps see
 * them. What's downloading now (and what failed) is kept in memory; what's
 * finished is the folder itself, so it's there after Wisp restarts. While a
 * download runs, a notification shows its progress; it opens the list.
 */
object Downloads {
    /** The folder under Download/ that Wisp saves into. */
    const val FOLDER = "Wisp"
    private const val CHANNEL = "downloads"

    enum class State { RUNNING, FAILED, CANCELLED }

    /** A download started this run: running, failed or cancelled. */
    class Job(val id: Int, val name: String, val mime: String, val total: Long) {
        @Volatile var done = 0L
        @Volatile var state = State.RUNNING
        @Volatile var error: String? = null
        @Volatile var cancel = false
    }

    /** A finished download: a file in Download/Wisp. */
    class File(val name: String, val mime: String, val size: Long, val modified: Long, val uri: Uri)

    val jobs = CopyOnWriteArrayList<Job>()
    private val listeners = CopyOnWriteArrayList<() -> Unit>()
    private val io = Executors.newCachedThreadPool()
    private val main = Handler(Looper.getMainLooper())
    private var nextId = 1000

    fun listen(f: () -> Unit) = listeners.add(f)
    fun unlisten(f: () -> Unit) = listeners.remove(f)
    private fun changed() = main.post { listeners.forEach { it() } }

    /** Save what `response` brings as `name`, in the background. */
    fun start(context: Context, response: WebResponse, name: String) {
        val app = context.applicationContext
        val mime = response.headers["Content-Type"]?.substringBefore(';')?.trim()?.ifEmpty { null } ?: "application/octet-stream"
        val total = response.headers["Content-Length"]?.toLongOrNull() ?: -1L
        val job = Job(nextId++, name, mime, total)
        jobs.add(0, job)
        changed()
        io.execute { run(app, job, response) }
    }

    private fun run(app: Context, job: Job, response: WebResponse) {
        var uri: Uri? = null
        var file: java.io.File? = null
        val result = runCatching {
            val input = response.body ?: error("the site sent nothing to save")
            input.use {
                val out = if (Build.VERSION.SDK_INT >= 29) {
                    val values = ContentValues().apply {
                        put(MediaStore.Downloads.DISPLAY_NAME, job.name)
                        put(MediaStore.Downloads.MIME_TYPE, job.mime)
                        put(MediaStore.Downloads.RELATIVE_PATH, "${Environment.DIRECTORY_DOWNLOADS}/$FOLDER")
                        put(MediaStore.Downloads.IS_PENDING, 1)
                    }
                    uri = app.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: error("can't create the file")
                    app.contentResolver.openOutputStream(uri!!) ?: error("can't write the file")
                } else {
                    file = java.io.File(legacyDir(app), job.name).also { it.parentFile?.mkdirs() }
                    file!!.outputStream()
                }
                out.use { o ->
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
            if (Build.VERSION.SDK_INT >= 29) {
                app.contentResolver.update(uri!!, ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }, null, null)
            }
        }
        val manager = app.getSystemService(NotificationManager::class.java)
        result.onSuccess {
            jobs.remove(job)
            notifyDone(app, job)
        }.onFailure { e ->
            // Nothing half-saved is left behind.
            uri?.let { u -> runCatching { app.contentResolver.delete(u, null, null) } }
            file?.delete()
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

    /** Where downloads go on Android 8 and 9 (without the storage permission). */
    private fun legacyDir(context: Context) =
        java.io.File(context.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: context.filesDir, FOLDER)

    /** The finished downloads, newest first. */
    fun files(context: Context): List<File> {
        if (Build.VERSION.SDK_INT < 29) {
            return legacyDir(context).listFiles().orEmpty().filter { it.isFile }.sortedByDescending { it.lastModified() }.map {
                File(it.name, mimeOf(it.name), it.length(), it.lastModified(),
                    FileProvider.getUriForFile(context, "${context.packageName}.files", it))
            }
        }
        val out = mutableListOf<File>()
        val columns = arrayOf(
            MediaStore.Downloads._ID, MediaStore.Downloads.DISPLAY_NAME, MediaStore.Downloads.MIME_TYPE,
            MediaStore.Downloads.SIZE, MediaStore.Downloads.DATE_MODIFIED,
        )
        val where = "${MediaStore.Downloads.RELATIVE_PATH} = ? AND ${MediaStore.Downloads.IS_PENDING} = 0"
        val args = arrayOf("${Environment.DIRECTORY_DOWNLOADS}/$FOLDER/")
        context.contentResolver.query(MediaStore.Downloads.EXTERNAL_CONTENT_URI, columns, where, args,
            "${MediaStore.Downloads.DATE_MODIFIED} DESC")?.use { c ->
            while (c.moveToNext()) {
                val id = c.getLong(0)
                out += File(c.getString(1) ?: "download", c.getString(2) ?: "application/octet-stream", c.getLong(3),
                    c.getLong(4) * 1000, ContentUris.withAppendedId(MediaStore.Downloads.EXTERNAL_CONTENT_URI, id))
            }
        }
        return out
    }

    fun delete(context: Context, file: File) {
        runCatching { context.contentResolver.delete(file.uri, null, null) }
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
        val n = builder(context, job.name, progressText(context, job)).setOngoing(true).setOnlyAlertOnce(true).apply {
            if (job.total > 0) setProgress(100, (job.done * 100 / job.total).toInt(), false) else setProgress(0, 0, true)
        }.build()
        runCatching { channel(context).notify(job.id, n) }
    }

    private fun notifyDone(context: Context, job: Job) {
        val n = builder(context, job.name, "Downloaded to ${Environment.DIRECTORY_DOWNLOADS}/$FOLDER")
            .setSmallIcon(android.R.drawable.stat_sys_download_done).setAutoCancel(true).build()
        runCatching { channel(context).notify(job.id, n) }
    }

    private fun notifyFailed(context: Context, job: Job) {
        val n = builder(context, job.name, "Download failed: ${job.error}")
            .setSmallIcon(android.R.drawable.stat_notify_error).setAutoCancel(true).build()
        runCatching { channel(context).notify(job.id, n) }
    }

    fun progressText(context: Context, job: Job): String {
        fun size(b: Long) = android.text.format.Formatter.formatShortFileSize(context, b)
        return if (job.total > 0) "${size(job.done)} of ${size(job.total)} · ${job.done * 100 / job.total}%" else size(job.done)
    }
}
