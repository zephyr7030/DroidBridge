package com.droidbridge.android.execution.shizuku

internal object ShizukuNativeLauncher {
    private var loaded = false

    @Synchronized
    fun ensureLoaded() {
        if (loaded) return
        System.loadLibrary("app_native")
        loaded = true
    }

    external fun nativeStart(
        clientId: String,
        executionId: String,
        nativeLibraryDirectory: String,
        guardPath: String,
        program: String,
        arguments: Array<String>,
        cwd: String,
        proofFd: Int,
        stdinFd: Int,
        stdoutFd: Int,
        stderrFd: Int,
    ): Long

    external fun nativeCloseLifetime(clientId: String, executionId: String, handle: Long): Boolean
    external fun nativeCancel(clientId: String, executionId: String, handle: Long): Boolean
    external fun nativeTimeout(clientId: String, executionId: String, handle: Long): Boolean
    external fun nativeWait(clientId: String, executionId: String, handle: Long): Int
    external fun nativeCloseClient(clientId: String): Int
    external fun nativeRename(source: String, destination: String, exchange: Boolean): Int
    external fun nativeReadDirectory(path: String, cookie: Long, limit: Int): ByteArray?
}
