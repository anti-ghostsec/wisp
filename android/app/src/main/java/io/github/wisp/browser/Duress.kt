package io.github.wisp.browser

import android.app.Activity
import android.app.ActivityManager
import android.content.Context
import android.os.Bundle
import android.util.Base64
import java.security.MessageDigest
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.PBEKeySpec

/**
 * A second password that, typed where Wisp asks for its password, wipes
 * everything on this phone (as Android's "Clear storage" does: the vault,
 * tabs, history, downloads, cookies, Tor's files, settings) and closes
 * Wisp, which then starts as new. What's on the sync service stays there.
 * Only a slow hash of it is kept, in the sealed settings.
 */
object Duress {
    private const val KEY = "duress"
    private const val ROUNDS = 310_000

    fun isSet(context: Context) = Engine.prefs(context).contains(KEY)

    fun set(context: Context, password: String?) {
        val prefs = Engine.prefs(context)
        if (password.isNullOrEmpty()) {
            prefs.edit().remove(KEY).apply()
            return
        }
        val salt = Sealed.random(16)
        val hash = derive(password, salt)
        prefs.edit().putString(KEY, b64(salt) + ":" + b64(hash)).apply()
    }

    /** Whether `password` is the duress password (slow: off the UI thread). */
    fun matches(context: Context, password: String): Boolean {
        val kept = Engine.prefs(context).getString(KEY, null) ?: return false
        val parts = kept.split(':')
        if (parts.size != 2) return false
        val salt = Base64.decode(parts[0], Base64.NO_WRAP)
        val want = Base64.decode(parts[1], Base64.NO_WRAP)
        return MessageDigest.isEqual(derive(password, salt), want)
    }

    /** Everything Wisp keeps on this phone, gone now; Wisp closes. */
    fun wipe(context: Context) {
        (context.getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager).clearApplicationUserData()
    }

    private fun derive(password: String, salt: ByteArray): ByteArray {
        val spec = PBEKeySpec(password.toCharArray(), salt, ROUNDS, 256)
        try {
            return SecretKeyFactory.getInstance("PBKDF2WithHmacSHA256").generateSecret(spec).encoded
        } finally {
            spec.clearPassword()
        }
    }

    private fun b64(b: ByteArray) = Base64.encodeToString(b, Base64.NO_WRAP)
}

/**
 * The "Wipe Wisp now" shortcut (long-press Wisp's icon): wipes everything
 * at once, without opening Wisp. Not exported: only the launcher, acting
 * for Wisp's own shortcut, can start it.
 */
class WipeActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Duress.wipe(this)
        finish()
    }
}
