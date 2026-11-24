package io.github.wisp.browser

import android.app.Activity
import android.content.Context
import android.content.res.Configuration
import android.graphics.Color
import android.os.Build
import android.view.WindowInsetsController

/**
 * Wisp's own colours, light or dark: dark when the phone is in dark mode,
 * or when the Dark mode plugin is on (it darkens pages, and Wisp around
 * them goes with it). Pages pick their own scheme from the same choice
 * (Engine's preferredColorScheme).
 */
class Palette(val dark: Boolean) {
    val background = if (dark) Color.rgb(0x12, 0x12, 0x14) else Color.WHITE
    val bar = if (dark) Color.rgb(0x2a, 0x2a, 0x2f) else Color.rgb(0xc0, 0xc0, 0xc0)
    val field = if (dark) Color.rgb(0x1c, 0x1c, 0x20) else Color.WHITE
    val text = if (dark) Color.rgb(0xec, 0xec, 0xee) else Color.BLACK
    val dim = if (dark) Color.rgb(0xb4, 0xb4, 0xba) else Color.DKGRAY
    val faint = if (dark) Color.rgb(0x8a, 0x8a, 0x90) else Color.GRAY
    val notice = if (dark) Color.rgb(0x3b, 0x35, 0x20) else Color.rgb(0xff, 0xff, 0xe1)
    val error = if (dark) Color.rgb(0xff, 0x85, 0x85) else Color.rgb(0xb0, 0, 0)

    /** Android's own look for menus and dialogs, to match. */
    val theme = if (dark) android.R.style.Theme_Material_NoActionBar else android.R.style.Theme_Material_Light_NoActionBar

    /** The theme, and Android's bars' icons light on dark or dark on light. */
    fun apply(activity: Activity) {
        activity.setTheme(theme)
        if (Build.VERSION.SDK_INT >= 30) {
            val light = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS
            activity.window.insetsController?.setSystemBarsAppearance(if (dark) 0 else light, light)
        }
    }

    companion object {
        fun of(context: Context): Palette {
            val night = (context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES
            val plugin = Engine.started && Engine.plugins.any { it.enabled && it.colorScheme == "dark" }
            return Palette(night || plugin)
        }
    }
}
