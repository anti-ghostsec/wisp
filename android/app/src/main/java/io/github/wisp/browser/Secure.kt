package io.github.wisp.browser

import android.app.AlertDialog
import android.content.Context
import android.view.View
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.widget.EditText

/** Whether screenshots of Wisp were allowed (Privacy & security). */
fun screenshotsAllowed(context: Context) =
    Engine.prefs(context).getBoolean("screenshots", false)

/**
 * Wisp's dialogs: like the window under them, no screenshots or screen
 * recording of them (a dialog is a window of its own, which the activity's
 * FLAG_SECURE doesn't cover), and nothing in them handed to autofill
 * services.
 */
class SecureDialog(context: Context) : AlertDialog.Builder(context) {
    override fun create(): AlertDialog = super.create().also { dialog ->
        if (!screenshotsAllowed(context)) dialog.window?.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        dialog.setOnShowListener { dialog.window?.decorView?.importantForAutofill = View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS }
    }
}

/**
 * A field the keyboard must not learn from (its incognito mode) and that
 * autofill services (a phone maker's password manager among them) never see.
 */
fun <T : EditText> T.private(): T = apply {
    imeOptions = imeOptions or EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
    importantForAutofill = View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS
}
