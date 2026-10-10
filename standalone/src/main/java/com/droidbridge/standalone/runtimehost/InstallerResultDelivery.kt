package com.droidbridge.standalone.runtimehost

import java.util.concurrent.Future
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** Retires a broadcast independently of a busy main thread or maintenance queue. */
internal class InstallerResultDelivery(
    private val scheduler: ScheduledExecutorService,
    private val nowMillis: () -> Long,
    private val budgetMillis: Long = 8_000,
) {
    fun submit(
        enqueue: (() -> Boolean, () -> Unit) -> Future<*>,
        finish: () -> Unit,
        failure: (String) -> Unit,
    ) {
        val deadline = nowMillis() + budgetMillis
        val completed = AtomicBoolean(false)
        val work = AtomicReference<Future<*>?>(null)
        val timeout = AtomicReference<ScheduledFuture<*>?>(null)
        val complete = {
            if (completed.compareAndSet(false, true)) {
                timeout.get()?.cancel(false)
                finish()
            }
        }
        try {
            timeout.set(scheduler.schedule({
                try {
                    if (!completed.get()) failure("INSTALLER_RESULT_TIMEOUT")
                } finally {
                    complete()
                    work.get()?.cancel(false)
                }
            }, budgetMillis, TimeUnit.MILLISECONDS))
            work.set(enqueue({ !completed.get() && nowMillis() < deadline }, complete))
            if (completed.get()) work.get()?.cancel(false)
        } catch (_: Exception) {
            try {
                failure("INSTALLER_RESULT_DELIVERY_FAILED")
            } finally {
                complete()
            }
        }
    }
}
