package com.droidbridge.standalone

import com.droidbridge.standalone.runtimehost.InstallerResultDelivery
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.After
import org.junit.Assert.*
import org.junit.Test

class I12_InstallerResultDeliveryTest {
    private val worker = Executors.newSingleThreadExecutor()
    private val timer = ScheduledThreadPoolExecutor(1).apply { removeOnCancelPolicy = true }
    private val delivery = InstallerResultDelivery(timer, { System.nanoTime() / 1_000_000 }, 500)

    @After
    fun stopWorkers() {
        worker.shutdownNow()
        timer.shutdownNow()
        assertTrue(worker.awaitTermination(5, TimeUnit.SECONDS))
        assertTrue(timer.awaitTermination(5, TimeUnit.SECONDS))
    }

    @Test
    fun queuedWorkIsCancelledAndBroadcastFinishesOnceAtTimeout() {
        val release = CountDownLatch(1)
        worker.submit { release.await() }
        val finished = CountDownLatch(1)
        val finishes = AtomicInteger()
        val launches = AtomicInteger()
        try {
            delivery.submit(
                enqueue = { canHandle, complete -> worker.submit {
                    if (canHandle()) launches.incrementAndGet()
                    complete()
                } },
                finish = { finishes.incrementAndGet(); finished.countDown() },
                failure = {},
            )
            assertTrue(finished.await(5, TimeUnit.SECONDS))
            release.countDown()
            worker.submit {}.get(5, TimeUnit.SECONDS)
            assertEquals(0, launches.get())
            assertEquals(1, finishes.get())
        } finally {
            release.countDown()
        }
    }

    @Test
    fun runningWorkCannotLaunchAfterDeadlineEvenWhenItsWorkerWasBlocked() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val finished = CountDownLatch(1)
        val launches = AtomicInteger()
        try {
            delivery.submit(
                enqueue = { canHandle, complete -> worker.submit {
                    entered.countDown()
                    release.await()
                    if (canHandle()) launches.incrementAndGet()
                    complete()
                } },
                finish = { finished.countDown() },
                failure = {},
            )
            assertTrue(entered.await(5, TimeUnit.SECONDS))
            assertTrue(finished.await(5, TimeUnit.SECONDS))
            release.countDown()
            worker.submit {}.get(5, TimeUnit.SECONDS)
            assertEquals(0, launches.get())
        } finally {
            release.countDown()
        }
    }

    @Test
    fun duplicateCompletionAndEnqueueFailureRetireTheirTimers() {
        val finishes = AtomicInteger()
        val failures = mutableListOf<String>()
        delivery.submit(
            enqueue = { canHandle, complete ->
                assertTrue(canHandle())
                complete()
                complete()
                CompletableFuture.completedFuture(Unit)
            },
            finish = { finishes.incrementAndGet() },
            failure = { failures += it },
        )
        assertEquals(1, finishes.get())
        assertTrue(timer.queue.isEmpty())
        delivery.submit(
            enqueue = { _, _ -> throw RejectedExecutionException() },
            finish = { finishes.incrementAndGet() },
            failure = { failures += it },
        )
        assertEquals(2, finishes.get())
        assertEquals(listOf("INSTALLER_RESULT_DELIVERY_FAILED"), failures)
        assertTrue(timer.queue.isEmpty())
    }
}
