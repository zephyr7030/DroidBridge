package com.droidbridge.android

import android.app.Application
import android.content.Context
import com.droidbridge.android.runtimehost.RuntimeProcessGraph
import rikka.shizuku.ShizukuProvider

class DroidBridgeApplication : Application() {
    private var processGraph: Any? = null

    override fun attachBaseContext(base: Context) {
        super.attachBaseContext(base)
        ShizukuProvider.enableMultiProcessSupport(
            Application.getProcessName() == base.packageName,
        )
    }

    override fun onCreate() {
        super.onCreate()
        processGraph = when (classifyProcess(Application.getProcessName(), packageName)) {
            ProcessRole.Default -> AppGraph(this)
            ProcessRole.Runtime -> RuntimeProcessGraph(this)
            ProcessRole.Unexpected -> null
        }
    }

    fun requireAppGraph(): AppGraph = processGraph as? AppGraph
        ?: error("default-process graph is unavailable")

    internal fun requireRuntimeGraph(): RuntimeProcessGraph = processGraph as? RuntimeProcessGraph
        ?: error("runtime-process graph is unavailable")
}

internal enum class ProcessRole { Default, Runtime, Unexpected }

internal fun classifyProcess(processName: String, packageName: String): ProcessRole = when (processName) {
    packageName -> ProcessRole.Default
    "$packageName:runtime" -> ProcessRole.Runtime
    else -> ProcessRole.Unexpected
}
