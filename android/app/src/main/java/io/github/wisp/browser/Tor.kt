package io.github.wisp.browser

import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.ServiceConnection
import android.os.IBinder
import androidx.localbroadcastmanager.content.LocalBroadcastManager
import org.torproject.jni.TorService

/**
 * Tor, built into Wisp (the Guardian Project's tor-android: the tor Orbot
 * runs). It runs inside Wisp while the route is Tor; Gecko is pointed at its
 * SOCKS port once it's connected, and at a closed port until then, so
 * nothing ever goes around it.
 */
object Tor {
    /** Tor's SOCKS port, once it's up (0 until then). */
    var port = 0
        private set
    var status = TorService.STATUS_OFF
        private set

    private var service: TorService? = null
    private var bound = false
    private var onChange: ((String) -> Unit)? = null

    private val receiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            status = intent.getStringExtra(TorService.EXTRA_STATUS) ?: return
            if (status == TorService.STATUS_ON) {
                port = service?.socksPort?.takeIf { it > 0 } ?: 0
            } else if (status != TorService.STATUS_STARTING) {
                port = 0
            }
            // Pages reload (onChange) only once Gecko is pointed at Tor.
            val changed = status
            Engine.applyPrefs(context).accept({ onChange?.invoke(changed) }, { onChange?.invoke(changed) })
        }
    }

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            service = (binder as TorService.LocalBinder).service
        }

        override fun onServiceDisconnected(name: ComponentName) {
            service = null
            port = 0
            status = TorService.STATUS_OFF
        }
    }

    val running get() = bound

    fun start(context: Context, onChange: (String) -> Unit) {
        this.onChange = onChange
        if (bound) return
        val app = context.applicationContext
        // Names are looked up through Tor (SOCKS requests that carry only an
        // address are refused), and nothing it can avoid goes to disk.
        TorService.getTorrc(app).writeText("SafeSocks 1\nAvoidDiskWrites 1\n")
        LocalBroadcastManager.getInstance(app).registerReceiver(receiver, IntentFilter(TorService.ACTION_STATUS))
        bound = app.bindService(Intent(app, TorService::class.java), connection, Context.BIND_AUTO_CREATE)
    }

    fun stop(context: Context) {
        if (!bound) return
        val app = context.applicationContext
        runCatching { app.unbindService(connection) }
        LocalBroadcastManager.getInstance(app).unregisterReceiver(receiver)
        bound = false
        service = null
        port = 0
        status = TorService.STATUS_OFF
        Engine.applyPrefs(app)
    }
}
