package io.github.wisp.browser

import android.app.Activity
import android.app.AlertDialog
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.text.format.DateFormat
import android.text.format.Formatter
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.BaseAdapter
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import java.util.Date

/**
 * Downloads: what's downloading now (with its progress, and Cancel), what
 * failed, and the files in Download/Wisp, to open, share or delete.
 */
class DownloadsActivity : Activity() {
    private lateinit var list: ListView
    private lateinit var empty: TextView
    private lateinit var clear: Button
    private var rows: List<Any> = emptyList()
    private val refresh = { reload() }

    private lateinit var palette: Palette

    private fun dp(n: Int) = (n * resources.displayMetrics.density).toInt()

    override fun onCreate(savedInstanceState: Bundle?) {
        palette = Palette.of(this)
        palette.apply(this)
        super.onCreate(savedInstanceState)
        if (!Engine.prefs(this).getBoolean("screenshots", false)) {
            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
        title = "Downloads"
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setBackgroundColor(palette.background) }
        val head = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(16), dp(16), dp(12), dp(4))
        }
        head.addView(TextView(this).apply {
            text = "Downloads"
            textSize = 22f
            setTextColor(palette.text)
        }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        clear = Button(this).apply { text = "Clear all"; setOnClickListener { askClear() } }
        head.addView(clear)
        root.addView(head)
        root.addView(TextView(this).apply {
            text = "Kept inside Wisp, where other apps can't see them, and deleted by Wipe everything. Open or share one, or save it to the phone to keep it outside Wisp."
            setTextColor(palette.dim)
            setPadding(dp(16), 0, dp(16), dp(8))
        })
        empty = TextView(this).apply {
            text = "Nothing downloaded yet."
            setTextColor(palette.faint)
            setPadding(dp(16), dp(24), dp(16), dp(16))
            visibility = View.GONE
        }
        root.addView(empty)
        list = ListView(this)
        root.addView(list, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        // Clear of Android's bars, as the browser is.
        root.setOnApplyWindowInsetsListener { v, insets ->
            if (Build.VERSION.SDK_INT >= 30) {
                val bars = insets.getInsets(android.view.WindowInsets.Type.systemBars())
                v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            }
            insets
        }
        setContentView(root)
        list.adapter = adapter
    }

    override fun onResume() {
        super.onResume()
        Downloads.listen(refresh)
        reload()
    }

    override fun onPause() {
        super.onPause()
        Downloads.unlisten(refresh)
    }

    private fun reload() {
        rows = Downloads.jobs.toList() + Downloads.files(this)
        empty.visibility = if (rows.isEmpty()) View.VISIBLE else View.GONE
        // Only when there's something it would clear (not just running ones).
        clear.visibility = if (rows.any { it !is Downloads.Job || it.state != Downloads.State.RUNNING }) View.VISIBLE else View.GONE
        adapter.notifyDataSetChanged()
    }

    private val adapter = object : BaseAdapter() {
        override fun getCount() = rows.size
        override fun getItem(position: Int) = rows[position]
        override fun getItemId(position: Int) = position.toLong()

        override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
            val row = rows[position]
            return LinearLayout(this@DownloadsActivity).apply {
                orientation = LinearLayout.VERTICAL
                setPadding(dp(16), dp(10), dp(12), dp(10))
                when (row) {
                    is Downloads.Job -> jobRow(this, row)
                    is Downloads.File -> fileRow(this, row)
                }
            }
        }
    }

    private fun label(text: String, size: Float, color: Int) = TextView(this).apply {
        this.text = text
        textSize = size
        setTextColor(color)
        maxLines = 2
    }

    private fun buttons(vararg items: Pair<String, () -> Unit>) = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.END
        for ((name, run) in items) addView(Button(this@DownloadsActivity).apply { text = name; isFocusable = false; setOnClickListener { run() } })
    }

    private fun jobRow(box: LinearLayout, job: Downloads.Job) {
        box.addView(label(job.name, 16f, palette.text))
        when (job.state) {
            Downloads.State.RUNNING -> {
                box.addView(ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply {
                    if (job.total > 0) {
                        max = 100
                        progress = (job.done * 100 / job.total).toInt()
                    } else {
                        isIndeterminate = true
                    }
                })
                box.addView(label(Downloads.progressText(this, job), 13f, palette.dim))
                box.addView(buttons("Cancel" to { job.cancel = true }))
            }
            else -> {
                box.addView(label("Failed: ${job.error ?: "cancelled"}", 13f, palette.error))
                box.addView(buttons("Remove" to { Downloads.forget(job) }))
            }
        }
    }

    private fun fileRow(box: LinearLayout, file: Downloads.File) {
        box.addView(label(file.name, 16f, palette.text))
        val date = DateFormat.getMediumDateFormat(this).format(Date(file.modified))
        box.addView(label("${Formatter.formatShortFileSize(this, file.size)} · $date", 13f, palette.dim))
        box.addView(buttons("Open" to { open(file) }, "Share" to { share(file) }, "Save…" to { export(file) }, "Delete" to { askDelete(file) }))
    }

    private fun open(file: Downloads.File) {
        val intent = Intent(Intent.ACTION_VIEW).setDataAndType(file.uri, file.mime).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        try {
            startActivity(intent)
        } catch (e: ActivityNotFoundException) {
            Toast.makeText(this, "No app on this phone opens ${file.mime} files", Toast.LENGTH_LONG).show()
        }
    }

    /** The file waiting for a place to be saved (Android's file picker answers in onActivityResult). */
    private var exporting: Downloads.File? = null

    /** Save a copy outside Wisp, wherever you pick in Android's file picker. */
    private fun export(file: Downloads.File) {
        exporting = file
        val pick = Intent(Intent.ACTION_CREATE_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType(file.mime)
            .putExtra(Intent.EXTRA_TITLE, file.name)
        try {
            startActivityForResult(pick, EXPORT)
        } catch (e: ActivityNotFoundException) {
            Toast.makeText(this, "This phone has no file picker to save with", Toast.LENGTH_LONG).show()
        }
    }

    @Deprecated("Activity's own; Wisp has no AndroidX activity")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        val file = exporting ?: return
        exporting = null
        val to = data?.data
        if (requestCode != EXPORT || resultCode != RESULT_OK || to == null) return
        Downloads.export(this, file, to) { e -> Toast.makeText(this, e?.let { "Saving failed: $it" } ?: "Saved", Toast.LENGTH_SHORT).show() }
    }

    private fun share(file: Downloads.File) {
        val send = Intent(Intent.ACTION_SEND).setType(file.mime).putExtra(Intent.EXTRA_STREAM, file.uri)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        startActivity(Intent.createChooser(send, "Share ${file.name}"))
    }

    private fun askDelete(file: Downloads.File) {
        SecureDialog(this)
            .setTitle("Delete ${file.name}?")
            .setMessage("It's deleted from Wisp. Copies you saved to the phone or shared stay where they are.")
            .setPositiveButton("Delete") { _, _ -> Downloads.delete(this, file) }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun askClear() {
        val files = Downloads.files(this).size
        SecureDialog(this)
            .setTitle("Clear all downloads?")
            .setMessage(
                (if (files > 0) "The $files downloaded file${if (files == 1) "" else "s"} here ${if (files == 1) "is" else "are"} deleted from Wisp, and failed downloads are taken off the list. " else "Failed downloads are taken off the list. ") +
                    "Downloads still running carry on. Copies you saved to the phone or shared stay where they are.",
            )
            .setPositiveButton("Clear all") { _, _ ->
                val n = Downloads.clear(this)
                Toast.makeText(this, if (n > 0) "Deleted $n file${if (n == 1) "" else "s"}" else "Cleared", Toast.LENGTH_SHORT).show()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private companion object {
        const val EXPORT = 1
    }
}
