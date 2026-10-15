package io.github.stroblme.accent

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder

/** Keep Android active during indexing with a foreground service and progress notification. */
class IndexingService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val text = intent?.getStringExtra(TEXT) ?: return START_NOT_STICKY
        // Refused once Android 15's daily allowance for a data sync is spent: the walk then goes
        // on as it did before this service, until the app is left.
        val type = ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
        runCatching { startForeground(ID, notification(this, text), type) }
            .onSuccess { running = true }
            .onFailure { stopSelf() }
        // A walk over before the service got here: let go now that letting go is allowed.
        if (!wanted) stopSelf()
        return START_NOT_STICKY
    }

    /** That allowance running out mid-walk: let go, and the walk finishes once the app is back. */
    override fun onTimeout(startId: Int, fgsType: Int) {
        running = false
        stopSelf()
    }

    override fun onDestroy() {
        running = false
        super.onDestroy()
    }

    companion object {
        private const val ID = 1
        private const val CHANNEL = "indexing"
        private const val TEXT = "text"

        /** Whether the service holds its notification, which is when the count may change. */
        @Volatile private var running = false

        /** Whether a walk is still running, which the service asks once it is up. */
        @Volatile private var wanted = false

        fun start(context: Context, text: String) {
            wanted = true
            val intent = Intent(context, IndexingService::class.java).putExtra(TEXT, text)
            // Refused from the background: the walk then runs as it did before this service.
            runCatching { context.startForegroundService(intent) }
        }

        fun update(context: Context, text: String) {
            if (!running) return
            context.getSystemService(NotificationManager::class.java)
                .notify(ID, notification(context, text))
        }

        /** Stop after foreground startup; stopping sooner can crash the app. */
        fun stop(context: Context) {
            wanted = false
            if (running) context.stopService(Intent(context, IndexingService::class.java))
        }

        /** Ongoing and quiet, the platform's indeterminate bar, and a tap back into the app. */
        private fun notification(context: Context, text: String): Notification {
            context.getSystemService(NotificationManager::class.java).createNotificationChannel(
                NotificationChannel(CHANNEL, "Indexing", NotificationManager.IMPORTANCE_LOW),
            )
            val app = Intent(context, MainActivity::class.java)
            return Notification.Builder(context, CHANNEL)
                .setSmallIcon(R.drawable.ic_launcher_monochrome)
                .setContentTitle(text)
                .setProgress(0, 0, true)
                .setOngoing(true)
                .setContentIntent(
                    PendingIntent.getActivity(context, 0, app, PendingIntent.FLAG_IMMUTABLE),
                )
                .build()
        }
    }
}

/** What the notification says: which vault, and how many files the walk has been through. */
internal fun indexingText(vault: String, files: Long): String =
    if (files > 0) "Indexing $vault… %,d files".format(files) else "Indexing $vault…"
