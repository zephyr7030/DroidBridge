package com.droidbridge.standalone.runtimehost

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.SystemClock
import android.util.Log
import com.droidbridge.standalone.DroidBridgeApplication
import java.util.concurrent.ScheduledThreadPoolExecutor

/** Results share the Runtime writer; package facts remain the sole proof of installation. */
class PackageInstallerResultReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val data = intent.data ?: return
        if (data.scheme != "droidbridge-update" || data.authority != "install" ||
            data.pathSegments.size != 2 || !intent.hasExtra(PackageInstaller.EXTRA_SESSION_ID)
        ) return
        val callback = UpdateInstallerCallback(
            data.pathSegments[0], data.pathSegments[1],
            intent.getIntExtra(PackageInstaller.EXTRA_SESSION_ID, -1),
            if (intent.hasExtra(PackageInstaller.EXTRA_STATUS)) intent.getIntExtra(PackageInstaller.EXTRA_STATUS, Int.MIN_VALUE) else null,
        )
        val confirmation = if (callback.status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
            runCatching { intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java) }.getOrNull()
        } else null
        val pending = goAsync()
        DELIVERY.submit(
            enqueue = { canHandle, finished ->
                val host = (context.applicationContext as DroidBridgeApplication).requireRuntimeGraph().hostController
                host.enqueueInstallerResult(
                    callback, canHandle,
                    confirmation?.let { activity ->
                        { context.startActivity(activity.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) }
                    },
                    finished,
                )
            },
            finish = pending::finish,
            failure = { Log.w(TAG, it) },
        )
    }

    private companion object {
        val DELIVERY = InstallerResultDelivery(
            ScheduledThreadPoolExecutor(1) { work ->
                Thread(work, "droidbridge-update-deadline").apply { isDaemon = true }
            }.apply { removeOnCancelPolicy = true },
            SystemClock::elapsedRealtime,
        )
        const val TAG = "DroidBridgeUpdate"
    }
}
