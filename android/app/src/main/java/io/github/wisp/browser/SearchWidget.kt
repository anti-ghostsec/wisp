package io.github.wisp.browser

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.content.Intent
import android.widget.RemoteViews

/**
 * The home screen's search bar: as wide as you stretch it. Touching it opens
 * a new tab in Wisp with the address bar ready and the keyboard up (home
 * screen widgets can't take typing themselves).
 */
class SearchWidget : AppWidgetProvider() {
    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) {
        val open = PendingIntent.getActivity(
            context,
            0,
            Intent(ACTION).setClass(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        for (id in ids) {
            val views = RemoteViews(context.packageName, R.layout.widget_search)
            views.setOnClickPendingIntent(R.id.widget_search, open)
            manager.updateAppWidget(id, views)
        }
    }

    companion object {
        const val ACTION = "io.github.wisp.browser.action.SEARCH"
    }
}
