package com.droidbridge.standalone

import com.droidbridge.standalone.runtimehost.DroidBridgeNotificationListenerService
import android.content.ComponentName
import android.app.Application
import com.droidbridge.standalone.client.DroidBridgeClient
import com.droidbridge.ui.product.about.LicenseEntry
import com.droidbridge.ui.product.about.ProductInfo
import com.droidbridge.ui.product.tasks.TaskRepository
import com.droidbridge.standalone.product.release.ReleaseConfig
import com.droidbridge.standalone.product.update.UpdateManager
import com.droidbridge.ui.diagnostics.DiagnosticsExporter
import com.droidbridge.ui.product.diagnostics.DiagnosticsExport
import java.io.File
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import com.droidbridge.ui.product.automation.AutomationRepository
import com.droidbridge.ui.product.settings.AppSettings

class AppGraph(application: Application) {
    val settings = AppSettings(application)
    val client = DroidBridgeClient(application)
    val automations = AutomationRepository(submit = { envelope -> client.submit(envelope) })
    val tasks = TaskRepository(submit = { envelope -> client.submit(envelope) })

    /** The App-owned canonical base whose S-SEC-005 fault files the default process reads directly. */
    val canonicalBase = File(application.createDeviceProtectedStorageContext().filesDir, "droidbridge")

    /** The S-UPD-001 update cache root that `Delete downloaded updates` removes. */
    val updateCache = File(application.cacheDir, "updates")

    /** Packaged release provenance for S-UI-017 Licenses, excluding build-only tools. */
    val licenses: List<LicenseEntry> by lazy {
        application.assets.open(ProductInfo.INVENTORY_ASSET).bufferedReader().use { reader ->
            ProductInfo.licenses(reader.readText())
        }
    }

    val apkVersionName: String =
        application.packageManager.getPackageInfo(application.packageName, 0).versionName.orEmpty()

    /** The process-scoped S-UPD-001 update state machine; debug builds are unconfigured. */
    val updates = UpdateManager(
        config = ReleaseConfig.from(
            BuildConfig.GITHUB_OWNER,
            BuildConfig.GITHUB_REPO,
            BuildConfig.RELEASE_MANIFEST_URL,
            BuildConfig.APK_SIGNER_SHA256,
            BuildConfig.RELEASE_KEY_ID,
            BuildConfig.RELEASE_PUBLIC_KEY_BASE64,
        ),
        installedVersionCode = application.packageManager.getPackageInfo(application.packageName, 0).longVersionCode,
        cacheRoot = updateCache,
    )

    /** The default-process S-SEC-005 export path shared by Diagnostics and MaintenanceRecovery. */
    val diagnosticsExporter: DiagnosticsExporter by lazy {
        val packageInfo = application.packageManager.getPackageInfo(application.packageName, 0)
        DiagnosticsExporter(
            client = client,
            readFaultFiles = { DiagnosticsExport.readFaultFiles(canonicalBase) },
            productVersions = buildJsonObject {
                put("apk_version_name", packageInfo.versionName.orEmpty())
                put("apk_version_code", packageInfo.longVersionCode)
            },
            releaseIdentifiers = buildJsonObject {
                put("application_id", application.packageName)
                put("build_type", BuildConfig.BUILD_TYPE)
            },
        )
    }

    /** The listener component whose notification-access page the setup guide opens. */
    val notificationListener: ComponentName =
        ComponentName(application, DroidBridgeNotificationListenerService::class.java)

    /** The exact packaged third-party notices text for the About dialog. */
    val thirdPartyNotices: String by lazy {
        application.assets.open(ProductInfo.NOTICES_ASSET).bufferedReader().use { it.readText() }
    }
}
