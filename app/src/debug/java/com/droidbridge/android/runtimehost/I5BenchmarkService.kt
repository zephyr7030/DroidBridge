package com.droidbridge.android.runtimehost

import android.app.Service
import android.content.Intent
import android.os.IBinder
import java.io.File

class I5BenchmarkService : Service() {
    private val binder = object : II5Benchmark.Stub() {
        override fun runBenchmark(): String {
            val deviceContext = createDeviceProtectedStorageContext()
            val base = File(deviceContext.filesDir, "droidbridge-i5-benchmark")
            return NativeRuntime.nativeRunI5DeviceBenchmark(base.absolutePath)
        }
    }

    override fun onBind(intent: Intent?): IBinder = binder
}
