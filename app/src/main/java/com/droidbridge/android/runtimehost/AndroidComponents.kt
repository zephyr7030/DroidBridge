package com.droidbridge.android.runtimehost

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.app.Notification
import android.app.PendingIntent
import android.content.ServiceConnection
import android.os.IBinder
import android.os.SystemClock
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import com.droidbridge.android.DroidBridgeApplication
import com.droidbridge.android.execution.android.AndroidExecutionException
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.BackgroundStartOptions
import com.droidbridge.android.execution.android.CapabilityRegistration
import com.droidbridge.android.execution.android.NotificationActionFact
import com.droidbridge.android.execution.android.NotificationOperationSurface
import com.droidbridge.android.execution.android.NotificationPlatform
import com.droidbridge.android.execution.android.ObservedNotification
import com.droidbridge.android.execution.android.RegisteredCapabilityState
import java.util.concurrent.atomic.AtomicLong

class DroidBridgeNotificationListenerService : NotificationListenerService() {
    private val componentGeneration = AtomicLong(SystemClock.elapsedRealtimeNanos())
    private var bound = false
    private val operations by lazy {
        NotificationOperationSurface(ListenerPlatform(), graph().hostController::validatesFence)
    }
    private val runtimeConnection = object : ServiceConnection {
        override fun onServiceConnected(name: android.content.ComponentName, service: IBinder) {
            publishConnection(true, componentGeneration.incrementAndGet())
        }

        override fun onServiceDisconnected(name: android.content.ComponentName) {
            publishConnection(false, componentGeneration.incrementAndGet())
        }
    }

    override fun onListenerConnected() {
        operations.connected(activeNotifications.orEmpty().map(::observe))
        if (!bound) {
            bound = bindService(
                Intent(this, DroidBridgeService::class.java),
                runtimeConnection,
                BIND_AUTO_CREATE,
            )
        } else {
            publishConnection(true, componentGeneration.incrementAndGet())
        }
    }

    private fun publishConnection(available: Boolean, generation: Long) {
        graph().androidExecutionRegistry.register(
            CapabilityRegistration(
                key = "android.notification_listener",
                state = if (available) {
                    RegisteredCapabilityState.Available
                } else {
                    RegisteredCapabilityState.Unavailable
                },
                reason = if (available) null else "LISTENER_DISCONNECTED",
                sourceGeneration = generation,
                executor = operations.takeIf { available },
                primitives = if (available) NOTIFICATION_PRIMITIVES else emptySet(),
            ),
        )
    }

    override fun onListenerDisconnected() {
        operations.disconnected()
        val generation = componentGeneration.incrementAndGet()
        if (bound) publishConnection(false, generation)
        if (bound) unbindService(runtimeConnection)
        bound = false
    }

    override fun onNotificationPosted(sbn: StatusBarNotification) {
        operations.posted(observe(sbn))
    }

    override fun onNotificationRemoved(sbn: StatusBarNotification) {
        operations.removed(sbn.key)
    }

    override fun onDestroy() {
        operations.disconnected()
        if (bound) unbindService(runtimeConnection)
        bound = false
        super.onDestroy()
    }

    private fun observe(sbn: StatusBarNotification): ObservedNotification<StatusBarNotification> {
        val notification = sbn.notification
        val extras = notification.extras
        return ObservedNotification(
            key = sbn.key,
            packageName = sbn.packageName,
            postedAtMillis = sbn.postTime,
            title = extras?.getCharSequence(Notification.EXTRA_TITLE)?.toString(),
            text = extras?.getCharSequence(Notification.EXTRA_TEXT)?.toString(),
            actions = notification.actions.orEmpty().map { action ->
                NotificationActionFact(
                    title = action.title?.toString(),
                    requiresRemoteInput = action.remoteInputs?.isNotEmpty() == true,
                )
            },
            handle = sbn,
        )
    }

    private inner class ListenerPlatform : NotificationPlatform<StatusBarNotification> {
        override fun cancel(key: String) {
            try {
                cancelNotification(key)
            } catch (_: SecurityException) {
                throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
            }
        }

        override fun sendAction(handle: StatusBarNotification, index: Int) {
            val intent = handle.notification.actions?.getOrNull(index)?.actionIntent
                ?: throw AndroidExecutionException("UNSUPPORTED")
            try {
                intent.send(
                    this@DroidBridgeNotificationListenerService,
                    0,
                    null,
                    null,
                    null,
                    null,
                    BackgroundStartOptions.sender(),
                )
            } catch (_: PendingIntent.CanceledException) {
                throw AndroidExecutionException("EXECUTION_FAILED")
            }
        }
    }

    private fun graph() = (application as DroidBridgeApplication).requireRuntimeGraph()

    private companion object {
        val NOTIFICATION_PRIMITIVES = setOf(
            AndroidPrimitive.NotificationSnapshot,
            AndroidPrimitive.NotificationDismiss,
            AndroidPrimitive.NotificationAction,
        )
    }
}

/**
 * Boot, package-replacement, time and timezone broadcasts only reload the owner/store and
 * reconcile the APK-hosted alarm from canonical due truth (S-LIFE-004); no tool side effect runs.
 */
class RuntimeReconcileReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action !in ACTIONS) return
        deliverAutomationWake(context, goAsync(), "automation_reconcile")
    }

    private companion object {
        val ACTIONS = setOf(
            Intent.ACTION_BOOT_COMPLETED,
            Intent.ACTION_MY_PACKAGE_REPLACED,
            Intent.ACTION_TIME_CHANGED,
            Intent.ACTION_TIMEZONE_CHANGED,
        )
    }
}

/**
 * The Shizuku user service's keep-alive wake: it runs as shell, which may send this broadcast but
 * may not start the non-exported Runtime service itself. The App is on the device-idle allowlist
 * by then, which is what admits the foreground-service start from the background.
 */
class KeepAliveWakeReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != DroidBridgeService.ACTION_KEEPALIVE_WAKE) return
        val started = runCatching {
            context.startForegroundService(
                Intent(context, DroidBridgeService::class.java).setAction(DroidBridgeService.ACTION_KEEPALIVE_WAKE),
            )
        }
        if (started.isFailure) NativeRuntime.nativeRecordHostFault("FGS_START_REJECTED", "keepalive_wake")
    }
}

/** The admitted wake trigger of the single APK exact alarm (S-LIFE-003). */
class ExactAlarmReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        deliverAutomationWake(context, goAsync(), "automation_exact_alarm")
    }
}

/**
 * Holds the broadcast only through bounded admission and re-arm: the keeper service is started
 * so admitted work keeps its Runtime process, then the scheduler rescans. A failure is recorded
 * instead of being dropped with the broadcast.
 */
private fun deliverAutomationWake(
    context: Context,
    pending: BroadcastReceiver.PendingResult,
    phase: String,
) {
    val application = context.applicationContext as DroidBridgeApplication
    Thread({
        try {
            val keeper = runCatching {
                context.startService(Intent(context, DroidBridgeService::class.java))
            }
            if (keeper.isFailure) {
                NativeRuntime.nativeRecordHostFault("FGS_START_REJECTED", phase)
            }
            if (!application.requireRuntimeGraph().hostController.wakeAutomation()) {
                NativeRuntime.nativeRecordHostFault("RUNTIME_UNAVAILABLE", phase)
            }
        } finally {
            pending.finish()
        }
    }, "droidbridge-automation-wake").start()
}
