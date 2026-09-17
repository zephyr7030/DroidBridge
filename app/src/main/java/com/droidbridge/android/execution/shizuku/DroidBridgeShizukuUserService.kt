package com.droidbridge.android.execution.shizuku

import android.content.Context
import android.os.Binder
import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.os.Process
import android.util.Log
import androidx.annotation.Keep
import java.io.File
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.SynchronousQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

class DroidBridgeShizukuUserService : IShizukuUserService.Stub {
    private val packageContext: Context?

    constructor() {
        packageContext = null
    }

    @Keep
    constructor(context: Context) {
        packageContext = context
    }

    private data class Client(
        val callerUid: Int,
        val clientId: String,
        val nativeLibraryDirectory: String,
        val guardPath: String,
        val deathRecipient: IBinder.DeathRecipient,
    )

    private data class Running(
        val clientId: String,
        val timeout: ScheduledFuture<*>,
    )

    private data class FsKey(val token: IBinder, val executionId: String)
    private data class FsCall(val descriptor: ParcelFileDescriptor?)

    private val ownership = ExecutionOwnershipRegistry<IBinder, Long>(ShizukuLaunchPolicy.MAX_GUARDS)
    private val clients = linkedMapOf<IBinder, Client>()
    private val running = ConcurrentHashMap<Long, Running>()
    private val fsCalls = ConcurrentHashMap<FsKey, FsCall>()
    private val waiters = ThreadPoolExecutor(
        0,
        ShizukuLaunchPolicy.MAX_GUARDS,
        30,
        TimeUnit.SECONDS,
        SynchronousQueue(),
    )
    private val deadlines = Executors.newSingleThreadScheduledExecutor()
    private val fsExecutor = ThreadPoolExecutor(
        4,
        4,
        30,
        TimeUnit.SECONDS,
        ArrayBlockingQueue(60),
    )

    /** Whether the App's Runtime asked to be started again when its process dies (an enabled agent connection). */
    private var keepAliveWanted = false
    private var keepAliveExempted = false
    private var unansweredWakes = 0

    override fun getUid(): Int = Process.myUid()

    /**
     * Keeps the App alive the way the Magisk module does, as far as shell can: the App is put on the
     * device-idle allowlist and allowed to run in the background, and a Runtime whose client token
     * dies is woken through the exported keep-alive receiver (shell may not start its service).
     */
    @Synchronized
    override fun setKeepAlive(token: IBinder?, enabled: Boolean) {
        requireClient(requireNotNull(token), Binder.getCallingUid())
        keepAliveWanted = enabled
        unansweredWakes = 0
        if (enabled && !keepAliveExempted) {
            val packageName = requireNotNull(packageContext).packageName
            keepAliveExempted = runShell("cmd", "deviceidle", "whitelist", "+$packageName") &&
                runShell("cmd", "appops", "set", packageName, "RUN_ANY_IN_BACKGROUND", "allow")
        }
    }

    @Synchronized
    private fun scheduleKeepAliveWake(delaySeconds: Long) {
        if (!keepAliveWanted || deadlines.isShutdown) return
        deadlines.schedule(::wakeIfAbsent, delaySeconds, TimeUnit.SECONDS)
    }

    @Synchronized
    private fun wakeIfAbsent() {
        if (!keepAliveWanted || clients.isNotEmpty()) return
        val packageName = requireNotNull(packageContext).packageName
        runShell(
            "am", "broadcast", "--user", "0", "-f", FLAG_INCLUDE_STOPPED_PACKAGES,
            "-a", KEEPALIVE_WAKE_ACTION, "-n", "$packageName/$KEEPALIVE_WAKE_RECEIVER",
        )
        val spacing = KEEPALIVE_SPACING_SECONDS[unansweredWakes.coerceAtMost(KEEPALIVE_SPACING_SECONDS.lastIndex)]
        unansweredWakes++
        scheduleKeepAliveWake(spacing)
    }

    private fun runShell(vararg command: String): Boolean {
        val outcome = runCatching {
            val process = ProcessBuilder(*command).redirectErrorStream(true)
                .redirectOutput(File("/dev/null")).start()
            if (!process.waitFor(KEEPALIVE_COMMAND_TIMEOUT_SECONDS, TimeUnit.SECONDS)) {
                process.destroyForcibly()
                error("timed out")
            }
            check(process.exitValue() == 0) { "exit ${process.exitValue()}" }
        }
        outcome.exceptionOrNull()?.let { Log.w(LOG_TAG, "keep-alive ${command.take(2).joinToString(" ")} failed: ${it.message}") }
        return outcome.isSuccess
    }

    @Synchronized
    override fun attachClient(
        token: IBinder?,
        clientId: String?,
    ) {
        val requiredToken = requireNotNull(token)
        val requiredClientId = requireNotNull(clientId)
        val context = requireNotNull(packageContext)
        val applicationInfo = context.packageManager.getApplicationInfo(
            context.packageName,
            android.content.pm.PackageManager.ApplicationInfoFlags.of(0),
        )
        val requiredDirectory = applicationInfo.nativeLibraryDir
        val requiredGuardPath = File(requiredDirectory, ShizukuLaunchPolicy.GUARD_NAME).absolutePath
        val callerUid = Binder.getCallingUid()
        if (callerUid != applicationInfo.uid) throw SecurityException("Shizuku caller UID rejected")
        require(clients.isEmpty())
        require(UUID.fromString(requiredClientId).toString() == requiredClientId)
        require(
            ShizukuLaunchPolicy.isAdmitted(
                Process.myUid(),
                callerUid,
                applicationInfo.uid,
                requiredDirectory,
                requiredGuardPath,
                ownership.activeCount,
            ),
        )
        ShizukuNativeLauncher.ensureLoaded()
        val deathRecipient = IBinder.DeathRecipient {
            detach(requiredToken, callerUid)
            // The Runtime process died rather than detaching: wake it again if it asked for that.
            scheduleKeepAliveWake(1)
        }
        requiredToken.linkToDeath(deathRecipient, 0)
        clients[requiredToken] = Client(
            callerUid,
            requiredClientId,
            requiredDirectory,
            requiredGuardPath,
            deathRecipient,
        )
        ownership.attach(requiredToken)
        unansweredWakes = 0
    }

    override fun detachClient(token: IBinder?) {
        val requiredToken = requireNotNull(token)
        detach(requiredToken, Binder.getCallingUid())
    }

    override fun executeGuarded(
        token: IBinder?,
        executionId: String?,
        primitive: String?,
        payload: ByteArray?,
        proofFd: ParcelFileDescriptor?,
        stdinFd: ParcelFileDescriptor?,
        stdoutFd: ParcelFileDescriptor?,
        stderrFd: ParcelFileDescriptor?,
        callback: IShizukuExecutionCallback?,
    ) {
        val descriptors = listOf(proofFd, stdinFd, stdoutFd, stderrFd)
        try {
            val requiredToken = requireNotNull(token)
            val requiredExecutionId = requireNotNull(executionId)
            val requiredCallback = requireNotNull(callback)
            val client = synchronized(this) { requireClient(requiredToken, Binder.getCallingUid()) }
            require(UUID.fromString(requiredExecutionId).toString() == requiredExecutionId)
            val plan = ShizukuGuardedPlanCodec.decode(
                requireNotNull(primitive),
                requireNotNull(payload),
                client.guardPath,
            )
            val requiredDescriptors = descriptors.map(::requireNotNull)
            val handle = start(
                requiredToken,
                client,
                requiredExecutionId,
                plan,
                requiredDescriptors,
            )
            if (handle == 0L) {
                runCatching { requiredCallback.onComplete(requiredExecutionId, -1, "EXECUTION_FAILED") }
                return
            }
            var timeout: ScheduledFuture<*>? = null
            try {
                timeout = deadlines.schedule(
                    {
                        if (ownership.ownedHandle(requiredToken, requiredExecutionId) == handle) {
                            ShizukuNativeLauncher.nativeTimeout(client.clientId, requiredExecutionId, handle)
                        }
                    },
                    plan.deadlineMs,
                    TimeUnit.MILLISECONDS,
                )
                running[handle] = Running(client.clientId, timeout)
                waiters.execute {
                    val exitCode = ShizukuNativeLauncher.nativeWait(client.clientId, requiredExecutionId, handle)
                    running.remove(handle)?.timeout?.cancel(false)
                    ownership.remove(requiredToken, requiredExecutionId)
                    runCatching {
                        requiredCallback.onComplete(
                            requiredExecutionId,
                            exitCode,
                            if (exitCode >= 0) "" else "IO_ERROR",
                        )
                    }
                }
            } catch (error: RuntimeException) {
                timeout?.cancel(false)
                running.remove(handle)
                ownership.remove(requiredToken, requiredExecutionId)
                ShizukuNativeLauncher.nativeCancel(client.clientId, requiredExecutionId, handle)
                ShizukuNativeLauncher.nativeWait(client.clientId, requiredExecutionId, handle)
                runCatching { requiredCallback.onComplete(requiredExecutionId, -1, "RESOURCE_LIMIT") }
            }
        } catch (error: SecurityException) {
            val id = executionId.orEmpty()
            runCatching { callback?.onComplete(id, -1, "PERMISSION_DENIED") }
        } catch (error: RuntimeException) {
            val id = executionId.orEmpty()
            runCatching { callback?.onComplete(id, -1, "INVALID_ARGUMENT") }
        } finally {
            descriptors.forEach { descriptor -> runCatching { descriptor?.close() } }
        }
    }

    override fun cancel(token: IBinder?, executionId: String?): Boolean {
        val requiredToken = requireNotNull(token)
        val requiredExecutionId = requireNotNull(executionId)
        require(UUID.fromString(requiredExecutionId).toString() == requiredExecutionId)
        val client = synchronized(this) { requireClient(requiredToken, Binder.getCallingUid()) }
        val handle = ownership.ownedHandle(requiredToken, requiredExecutionId) ?: return false
        return ShizukuNativeLauncher.nativeCancel(client.clientId, requiredExecutionId, handle)
    }

    override fun executeFs(
        token: IBinder?,
        executionId: String?,
        payload: ByteArray?,
        descriptor: ParcelFileDescriptor?,
        callback: IShizukuFsCallback?,
    ) {
        var fsKey: FsKey? = null
        try {
            val requiredToken = requireNotNull(token)
            val requiredExecutionId = requireNotNull(executionId)
            val requiredCallback = requireNotNull(callback)
            val callerUid = Binder.getCallingUid()
            synchronized(this) { requireClient(requiredToken, callerUid) }
            require(UUID.fromString(requiredExecutionId).toString() == requiredExecutionId)
            val operation = ShizukuFsCodec.decode(requireNotNull(payload))
            val key = FsKey(requiredToken, requiredExecutionId)
            require(fsCalls.putIfAbsent(key, FsCall(descriptor)) == null)
            fsKey = key
            fsExecutor.execute {
                var outputDescriptor: ParcelFileDescriptor? = null
                try {
                    synchronized(this) { requireClient(requiredToken, callerUid) }
                    val result = ShizukuFsExecutor.execute(operation, descriptor)
                    outputDescriptor = result.descriptor
                    fsCalls.remove(key)?.let { call -> runCatching { call.descriptor?.close() } }
                    requiredCallback.onComplete(
                        requiredExecutionId,
                        result.payload,
                        outputDescriptor,
                        "",
                    )
                } catch (error: Exception) {
                    fsCalls.remove(key)?.let { call -> runCatching { call.descriptor?.close() } }
                    runCatching {
                        requiredCallback.onComplete(
                            requiredExecutionId,
                            ByteArray(0),
                            null,
                            ShizukuFsExecutor.errorCode(error),
                        )
                    }
                } finally {
                    runCatching { outputDescriptor?.close() }
                    fsCalls.remove(key)?.let { call -> runCatching { call.descriptor?.close() } }
                }
            }
        } catch (error: RuntimeException) {
            fsKey?.let { key -> fsCalls.remove(key) }
            runCatching {
                callback?.onComplete(
                    executionId.orEmpty(),
                    ByteArray(0),
                    null,
                    when (error) {
                        is SecurityException -> "PERMISSION_DENIED"
                        is java.util.concurrent.RejectedExecutionException -> "RESOURCE_LIMIT"
                        else -> ShizukuFsExecutor.errorCode(error)
                    },
                )
            }
            runCatching { descriptor?.close() }
        }
    }

    override fun destroy() {
        val tokens = synchronized(this) { clients.keys.toList() }
        tokens.forEach { token ->
            val uid = synchronized(this) { clients[token]?.callerUid } ?: return@forEach
            detach(token, uid)
        }
        deadlines.shutdownNow()
        waiters.shutdown()
        fsExecutor.shutdown()
        System.exit(0)
    }

    @Synchronized
    private fun start(
        token: IBinder,
        client: Client,
        executionId: String,
        plan: ShizukuGuardedPlan,
        descriptors: List<ParcelFileDescriptor>,
    ): Long {
        require(ownership.ownedHandle(token, executionId) == null)
        require(ownership.activeCount < ShizukuLaunchPolicy.MAX_GUARDS)
        val handle = ShizukuNativeLauncher.nativeStart(
            client.clientId,
            executionId,
            client.nativeLibraryDirectory,
            client.guardPath,
            plan.program,
            plan.arguments.toTypedArray(),
            plan.cwd,
            descriptors[0].fd,
            descriptors[1].fd,
            descriptors[2].fd,
            descriptors[3].fd,
        )
        if (handle == 0L) return 0
        if (ownership.admit(token, executionId, handle)) return handle
        ShizukuNativeLauncher.nativeCancel(client.clientId, executionId, handle)
        ShizukuNativeLauncher.nativeWait(client.clientId, executionId, handle)
        return 0
    }

    @Synchronized
    private fun detach(token: IBinder, callerUid: Int) {
        val client = clients[token] ?: return
        if (client.callerUid != callerUid) throw SecurityException("Shizuku caller UID rejected")
        clients.remove(token)
        runCatching { token.unlinkToDeath(client.deathRecipient, 0) }
        ownership.detach(token)
        ShizukuNativeLauncher.nativeCloseClient(client.clientId)
        running.entries.removeIf { (_, execution) ->
            if (execution.clientId != client.clientId) return@removeIf false
            execution.timeout.cancel(false)
            true
        }
        fsCalls.entries.forEach { (key, call) ->
            if (key.token == token && fsCalls.remove(key, call)) {
                runCatching { call.descriptor?.close() }
            }
        }
    }

    @Synchronized
    private fun requireClient(token: IBinder, callerUid: Int): Client {
        val client = clients[token] ?: throw SecurityException("Shizuku client token rejected")
        if (client.callerUid != callerUid) throw SecurityException("Shizuku caller UID rejected")
        return client
    }

    private companion object {
        const val LOG_TAG = "DroidBridgeShizuku"
        const val KEEPALIVE_WAKE_ACTION = "com.droidbridge.android.action.KEEPALIVE_WAKE"
        const val KEEPALIVE_WAKE_RECEIVER = "com.droidbridge.android.runtimehost.KeepAliveWakeReceiver"
        const val FLAG_INCLUDE_STOPPED_PACKAGES = "32"
        const val KEEPALIVE_COMMAND_TIMEOUT_SECONDS = 10L
        val KEEPALIVE_SPACING_SECONDS = longArrayOf(15, 60, 180, 300)
    }
}
