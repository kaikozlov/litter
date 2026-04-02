package com.litter.android.push

import android.app.PendingIntent
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Intent
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.core.app.NotificationCompat
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import com.litter.android.MainActivity

class LitterFirebaseMessagingService : FirebaseMessagingService() {
    companion object {
        private const val CHANNEL_ID = "turn_status"
        private const val NOTIFICATION_ID = 9001
    }

    private val mainHandler = Handler(Looper.getMainLooper())

    override fun onMessageReceived(remoteMessage: RemoteMessage) {
        val data = remoteMessage.data
        when (data["type"]) {
            "turn_keepalive" -> showOrUpdateTurnNotification(data)
            "turn_end" -> showTurnCompleteNotification(data)
        }
    }

    override fun onNewToken(token: String) {
        getSharedPreferences("litter_push", MODE_PRIVATE)
            .edit()
            .putString("fcm_token", token)
            .apply()
    }

    private fun showOrUpdateTurnNotification(data: Map<String, String>) {
        ensureChannel()
        val phase = data["phase"] ?: "thinking"
        val elapsed = data["elapsedSeconds"]?.toLongOrNull() ?: 0
        val toolCount = data["toolCallCount"]?.toIntOrNull() ?: 0
        val minutes = elapsed / 60
        val seconds = elapsed % 60
        val text = buildString {
            append("Phase: $phase")
            append(" | ${minutes}m ${seconds}s")
            if (toolCount > 0) append(" | $toolCount tools")
        }
        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_popup_sync)
            .setContentTitle("Codex turn in progress")
            .setContentText(text)
            .setContentIntent(notificationContentIntent(data))
            .setOngoing(true)
            .setSilent(true)
            .setAutoCancel(false)
            .build()
        val nm = getSystemService(NotificationManager::class.java)
        nm.notify(NOTIFICATION_ID, notification)
    }

    private fun showTurnCompleteNotification(data: Map<String, String>) {
        ensureChannel()
        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_popup_sync)
            .setContentTitle("Codex turn completed")
            .setContentText(data["summary"] ?: "Turn finished")
            .setContentIntent(notificationContentIntent(data))
            .setOngoing(false)
            .setAutoCancel(true)
            .build()
        val nm = getSystemService(NotificationManager::class.java)
        nm.notify(NOTIFICATION_ID, notification)
        mainHandler.postDelayed({ nm.cancel(NOTIFICATION_ID) }, 10_000)
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "Turn Status",
                NotificationManager.IMPORTANCE_LOW,
            )
            getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
        }
    }

    private fun notificationContentIntent(data: Map<String, String>): PendingIntent? {
        val serverId = data["serverId"] ?: data["server_id"] ?: return null
        val threadId = data["threadId"] ?: data["thread_id"] ?: return null
        if (serverId.isBlank() || threadId.isBlank()) {
            return null
        }

        val intent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP
            putExtra(MainActivity.EXTRA_NOTIFICATION_SERVER_ID, serverId)
            putExtra(MainActivity.EXTRA_NOTIFICATION_THREAD_ID, threadId)
        }
        val requestCode = (serverId + ":" + threadId).hashCode()
        return PendingIntent.getActivity(
            this,
            requestCode,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }
}
