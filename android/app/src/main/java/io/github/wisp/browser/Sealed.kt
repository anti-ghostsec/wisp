package io.github.wisp.browser

import android.content.Context
import android.content.SharedPreferences
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Files only this phone can read: AES-GCM with a key in Android's keystore
 * (in its secure hardware where there is some), which never leaves it and
 * goes when Wisp's data is cleared. For what Wisp keeps outside the vault
 * because it's needed before the vault is unlocked: its settings (the
 * route, Tor's bridges) and Tor's own files.
 */
object Sealed {
    private const val ALIAS = "wisp-local"
    private const val TAG = "wisp"

    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return gen.generateKey()
    }

    /** The nonce (12 bytes), then the ciphertext and its tag. */
    fun seal(plain: ByteArray): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key())
        return cipher.iv + cipher.doFinal(plain)
    }

    fun open(sealed: ByteArray): ByteArray {
        require(sealed.size > 12) { "too short" }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, sealed.copyOfRange(0, 12)))
        return cipher.doFinal(sealed, 12, sealed.size - 12)
    }

    /** Write `plain` sealed to `file`, replacing it whole (a new file, then renamed). */
    fun write(file: File, plain: ByteArray) {
        file.parentFile?.mkdirs()
        val tmp = File(file.path + ".new")
        tmp.writeBytes(seal(plain))
        if (!tmp.renameTo(file)) {
            tmp.delete()
            throw java.io.IOException("can't replace ${file.name}")
        }
    }

    /** What `file` holds, or null if it isn't there or can't be opened (the key is gone). */
    fun read(file: File): ByteArray? {
        if (!file.exists()) return null
        return try {
            open(file.readBytes())
        } catch (e: Exception) {
            Log.w(TAG, "can't open ${file.name}: $e")
            null
        }
    }

    /** Random bytes. */
    fun random(n: Int) = ByteArray(n).also { SecureRandom().nextBytes(it) }
}

/**
 * Wisp's settings, sealed ([Sealed]) in one file instead of Android's
 * plain XML preferences: which route and bridges you use isn't left
 * readable on the phone. The same interface as SharedPreferences, so the
 * rest of Wisp is unchanged; settings kept by older Wisps move in once.
 */
class SealedPrefs private constructor(private val file: File) : SharedPreferences {
    private val values = HashMap<String, Any>()
    private val listeners = mutableSetOf<SharedPreferences.OnSharedPreferenceChangeListener>()

    private fun load() {
        val text = Sealed.read(file)?.toString(Charsets.UTF_8) ?: return
        val json = runCatching { JSONObject(text) }.getOrNull() ?: return
        for (k in json.keys()) {
            val e = json.optJSONObject(k) ?: continue
            val v: Any = when (e.optString("t")) {
                "s" -> e.optString("v")
                "b" -> e.optBoolean("v")
                "i" -> e.optInt("v")
                "l" -> e.optLong("v")
                "f" -> e.optDouble("v").toFloat()
                "set" -> e.optJSONArray("v")?.let { a -> (0 until a.length()).map { a.getString(it) }.toMutableSet() } ?: mutableSetOf<String>()
                else -> continue
            }
            values[k] = v
        }
    }

    @Synchronized
    private fun save() {
        val json = JSONObject()
        for ((k, v) in values) {
            val e = JSONObject()
            when (v) {
                is String -> e.put("t", "s").put("v", v)
                is Boolean -> e.put("t", "b").put("v", v)
                is Int -> e.put("t", "i").put("v", v)
                is Long -> e.put("t", "l").put("v", v)
                is Float -> e.put("t", "f").put("v", v.toDouble())
                is Set<*> -> e.put("t", "set").put("v", JSONArray(v.map { it.toString() }))
                else -> continue
            }
            json.put(k, e)
        }
        try {
            Sealed.write(file, json.toString().toByteArray(Charsets.UTF_8))
        } catch (e: Exception) {
            Log.e("wisp", "can't save settings: $e")
        }
    }

    @Synchronized private fun value(key: String?): Any? = if (key == null) null else values[key]

    @Synchronized override fun getAll(): MutableMap<String, *> = HashMap<String, Any>(values)
    override fun getString(key: String?, defValue: String?): String? = (value(key) as? String) ?: defValue
    override fun getStringSet(key: String?, defValues: MutableSet<String>?): MutableSet<String>? =
        (value(key) as? Set<*>)?.map { it.toString() }?.toMutableSet() ?: defValues
    override fun getInt(key: String?, defValue: Int): Int = (value(key) as? Int) ?: defValue
    override fun getLong(key: String?, defValue: Long): Long = (value(key) as? Long) ?: defValue
    override fun getFloat(key: String?, defValue: Float): Float = (value(key) as? Float) ?: defValue
    override fun getBoolean(key: String?, defValue: Boolean): Boolean = (value(key) as? Boolean) ?: defValue
    override fun contains(key: String?): Boolean = value(key) != null

    override fun edit(): SharedPreferences.Editor = Editor()

    override fun registerOnSharedPreferenceChangeListener(listener: SharedPreferences.OnSharedPreferenceChangeListener?) {
        if (listener != null) synchronized(listeners) { listeners.add(listener) }
    }

    override fun unregisterOnSharedPreferenceChangeListener(listener: SharedPreferences.OnSharedPreferenceChangeListener?) {
        if (listener != null) synchronized(listeners) { listeners.remove(listener) }
    }

    private inner class Editor : SharedPreferences.Editor {
        private val puts = HashMap<String, Any?>()
        private var clearAll = false

        override fun putString(key: String?, value: String?) = put(key, value)
        override fun putStringSet(key: String?, values: MutableSet<String>?) = put(key, values?.toMutableSet())
        override fun putInt(key: String?, value: Int) = put(key, value)
        override fun putLong(key: String?, value: Long) = put(key, value)
        override fun putFloat(key: String?, value: Float) = put(key, value)
        override fun putBoolean(key: String?, value: Boolean) = put(key, value)
        override fun remove(key: String?) = put(key, null)
        override fun clear(): SharedPreferences.Editor {
            clearAll = true
            return this
        }

        private fun put(key: String?, value: Any?): SharedPreferences.Editor {
            if (key != null) puts[key] = value
            return this
        }

        override fun commit(): Boolean {
            val changed: List<String>
            synchronized(this@SealedPrefs) {
                if (clearAll) values.clear()
                for ((k, v) in puts) {
                    if (v == null) values.remove(k) else values[k] = v
                }
                changed = puts.keys.toList()
                save()
            }
            val those = synchronized(listeners) { listeners.toList() }
            for (k in changed) those.forEach { it.onSharedPreferenceChanged(this@SealedPrefs, k) }
            return true
        }

        // The file is small: written now, as commit does.
        override fun apply() {
            commit()
        }
    }

    companion object {
        private var instance: SealedPrefs? = null

        /** Wisp's settings; the first time, those older Wisps kept in plain XML move in. */
        @Synchronized
        fun get(context: Context): SealedPrefs {
            instance?.let { return it }
            val app = context.applicationContext
            val prefs = SealedPrefs(File(app.filesDir, "settings.sealed"))
            prefs.load()
            val old = app.getSharedPreferences(Engine.PREFS, Context.MODE_PRIVATE)
            val kept = old.all
            if (kept.isNotEmpty()) {
                synchronized(prefs) {
                    for ((k, v) in kept) {
                        if (v != null && !prefs.values.containsKey(k)) prefs.values[k] = v
                    }
                    prefs.save()
                }
                old.edit().clear().commit()
                app.deleteSharedPreferences(Engine.PREFS)
            }
            instance = prefs
            return prefs
        }
    }
}
