package io.github.wisp.browser

import android.app.AlertDialog
import android.text.InputType
import android.widget.EditText
import android.widget.LinearLayout
import org.mozilla.geckoview.AllowOrDeny
import org.mozilla.geckoview.GeckoResult
import org.mozilla.geckoview.GeckoSession
import org.mozilla.geckoview.GeckoSession.PromptDelegate
import org.mozilla.geckoview.GeckoSession.PromptDelegate.PromptResponse

/** Pages' dialogs: alert, confirm, prompt, <select> menus, sign-in boxes, and held pop-ups. */
class Prompts(private val activity: MainActivity, private val tab: Tab) : PromptDelegate {
    private fun dialog(title: String?) = AlertDialog.Builder(activity).setTitle(title?.takeIf { it.isNotEmpty() })

    override fun onAlertPrompt(session: GeckoSession, prompt: PromptDelegate.AlertPrompt): GeckoResult<PromptResponse> {
        val result = GeckoResult<PromptResponse>()
        dialog(prompt.title).setMessage(prompt.message)
            .setPositiveButton("OK") { _, _ -> result.complete(prompt.dismiss()) }
            .setOnCancelListener { result.complete(prompt.dismiss()) }
            .show()
        return result
    }

    override fun onButtonPrompt(session: GeckoSession, prompt: PromptDelegate.ButtonPrompt): GeckoResult<PromptResponse> {
        val result = GeckoResult<PromptResponse>()
        dialog(prompt.title).setMessage(prompt.message)
            .setPositiveButton("OK") { _, _ -> result.complete(prompt.confirm(PromptDelegate.ButtonPrompt.Type.POSITIVE)) }
            .setNegativeButton("Cancel") { _, _ -> result.complete(prompt.confirm(PromptDelegate.ButtonPrompt.Type.NEGATIVE)) }
            .setOnCancelListener { result.complete(prompt.dismiss()) }
            .show()
        return result
    }

    override fun onTextPrompt(session: GeckoSession, prompt: PromptDelegate.TextPrompt): GeckoResult<PromptResponse> {
        val result = GeckoResult<PromptResponse>()
        val field = EditText(activity).apply { setText(prompt.defaultValue.orEmpty()) }
        dialog(prompt.title).setMessage(prompt.message).setView(field)
            .setPositiveButton("OK") { _, _ -> result.complete(prompt.confirm(field.text.toString())) }
            .setNegativeButton("Cancel") { _, _ -> result.complete(prompt.dismiss()) }
            .setOnCancelListener { result.complete(prompt.dismiss()) }
            .show()
        return result
    }

    override fun onAuthPrompt(session: GeckoSession, prompt: PromptDelegate.AuthPrompt): GeckoResult<PromptResponse> {
        val result = GeckoResult<PromptResponse>()
        val user = EditText(activity).apply { hint = "User name"; setText(prompt.authOptions.username.orEmpty()) }
        val pass = EditText(activity).apply { hint = "Password"; inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD }
        val form = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL; setPadding(48, 16, 48, 0); addView(user); addView(pass) }
        dialog(prompt.title ?: "Sign in").setMessage(prompt.message).setView(form)
            .setPositiveButton("Sign in") { _, _ -> result.complete(prompt.confirm(user.text.toString(), pass.text.toString())) }
            .setNegativeButton("Cancel") { _, _ -> result.complete(prompt.dismiss()) }
            .setOnCancelListener { result.complete(prompt.dismiss()) }
            .show()
        return result
    }

    override fun onChoicePrompt(session: GeckoSession, prompt: PromptDelegate.ChoicePrompt): GeckoResult<PromptResponse> {
        val result = GeckoResult<PromptResponse>()
        var answered = false
        // Groups (<optgroup>) flattened; separators and disabled entries left out.
        val choices = prompt.choices.flatMap { c -> if (c.items != null) c.items!!.toList() else listOf(c) }
            .filter { !it.separator && !it.disabled }
        val labels = choices.map { it.label }.toTypedArray()
        val d = dialog(prompt.title ?: prompt.message)
        if (prompt.type == PromptDelegate.ChoicePrompt.Type.MULTIPLE) {
            val checked = choices.map { it.selected }.toBooleanArray()
            d.setMultiChoiceItems(labels, checked) { _, i, on -> checked[i] = on }
                .setPositiveButton("OK") { _, _ ->
                    answered = true
                    result.complete(prompt.confirm(choices.filterIndexed { i, _ -> checked[i] }.toTypedArray()))
                }
        } else {
            d.setSingleChoiceItems(labels, choices.indexOfFirst { it.selected }) { dlg, i ->
                answered = true
                result.complete(prompt.confirm(choices[i]))
                dlg.dismiss()
            }
        }
        d.setOnCancelListener { if (!answered) result.complete(prompt.dismiss()) }.show()
        return result
    }

    override fun onPopupPrompt(session: GeckoSession, prompt: PromptDelegate.PopupPrompt): GeckoResult<PromptResponse> {
        val result = GeckoResult<PromptResponse>()
        activity.popupBlocked(tab, prompt.targetUri) { open ->
            result.complete(prompt.confirm(if (open) AllowOrDeny.ALLOW else AllowOrDeny.DENY))
        }
        return result
    }

    override fun onBeforeUnloadPrompt(session: GeckoSession, prompt: PromptDelegate.BeforeUnloadPrompt): GeckoResult<PromptResponse> {
        val result = GeckoResult<PromptResponse>()
        dialog("Leave this page?").setMessage("Changes you made may not be saved.")
            .setPositiveButton("Leave") { _, _ -> result.complete(prompt.confirm(AllowOrDeny.ALLOW)) }
            .setNegativeButton("Stay") { _, _ -> result.complete(prompt.confirm(AllowOrDeny.DENY)) }
            .setOnCancelListener { result.complete(prompt.confirm(AllowOrDeny.DENY)) }
            .show()
        return result
    }

    override fun onRepostConfirmPrompt(session: GeckoSession, prompt: PromptDelegate.RepostConfirmPrompt): GeckoResult<PromptResponse> {
        val result = GeckoResult<PromptResponse>()
        dialog("Send the form again?").setMessage("The page asks to send what you entered again.")
            .setPositiveButton("Send") { _, _ -> result.complete(prompt.confirm(AllowOrDeny.ALLOW)) }
            .setNegativeButton("Cancel") { _, _ -> result.complete(prompt.confirm(AllowOrDeny.DENY)) }
            .setOnCancelListener { result.complete(prompt.confirm(AllowOrDeny.DENY)) }
            .show()
        return result
    }

    override fun onFilePrompt(session: GeckoSession, prompt: PromptDelegate.FilePrompt): GeckoResult<PromptResponse> {
        val result = GeckoResult<PromptResponse>()
        val multiple = prompt.type == PromptDelegate.FilePrompt.Type.MULTIPLE
        activity.chooseFiles(prompt.mimeTypes, multiple) { uris ->
            result.complete(
                when {
                    uris.isNullOrEmpty() -> prompt.dismiss()
                    multiple -> prompt.confirm(activity, uris.toTypedArray())
                    else -> prompt.confirm(activity, uris[0])
                },
            )
        }
        return result
    }
}
