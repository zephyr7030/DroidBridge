package com.droidbridge.android.runtimehost

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import com.droidbridge.android.product.release.ReleaseArtifact
import com.droidbridge.android.product.release.ReleaseConfig
import com.droidbridge.android.product.release.ReleaseHash
import com.droidbridge.android.product.release.ReleaseManifests
import com.droidbridge.android.product.release.ReleaseRejected
import java.io.File
import java.security.MessageDigest
import java.time.Instant
import java.time.temporal.ChronoUnit
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** The stable module fact HostController observes from the authenticated companion connection. */
internal enum class ModuleObservation(val wire: String) {
    Compatible("compatible"),
    Absent("absent"),
    Mismatched("mismatched"),
    Excluded("excluded"),
}

internal enum class PrivilegedArtifact { Apk, Module }

/** One daemon-owned attempt: the installer exit code when it ran, and whether guard cleanup is proven. */
internal data class PrivilegedOutcome(val exitCode: Int?, val cleanupVerified: Boolean)

/** HostController facilities the maintenance flow needs; every call runs on its transition executor. */
internal interface MaintenanceHost {
    /** Makes APK Runtime the active host; returns a refusal code or null. */
    fun ensureApkHost(): String?

    /** Closes business admission after proving zero work; returns a refusal code or null. */
    fun closeAdmission(): String?

    fun reopenAdmission(): Boolean

    fun moduleObservation(): ModuleObservation

    fun cleanupVerified(): Boolean

    /** The authenticated compatible daemon can run the fixed S-IPC-DAEMON-005 installers. */
    fun privilegedInstallAvailable(): Boolean

    /** Dispatches exactly one daemon install of the recorded attempt with a read-only artifact descriptor. */
    fun privilegedInstall(kind: PrivilegedArtifact, record: UpdateMaintenanceRecord, artifact: File): PrivilegedOutcome

    /** The daemon's `MaintenanceStatus` cleanup token for [updateId], or null when no daemon answers. */
    fun privilegedCleanup(updateId: String): String?
}

internal data class ArchiveFacts(val packageName: String, val versionCode: Long, val signerSha256: String?)

internal interface InstalledPackageFacts {
    fun installedVersionCode(): Long

    fun installedSignerSha256(): String?

    /** Package identity of an APK file, or null when Android cannot parse it. */
    fun archive(file: File): ArchiveFacts?
}

internal interface ApkSessionInstaller {
    fun abandonUnrecordedSessions(recorded: Int?)

    fun create(size: Long): Int

    fun exists(sessionId: Int): Boolean

    /** Streams exactly the verified bytes into the session, then commits it for user confirmation. */
    fun writeAndCommit(sessionId: Int, apk: File, size: Long, sha256: String)

    fun abandon(sessionId: Int)
}

private class MaintenanceRefused(val code: String, message: String) : Exception(message)

/**
 * The APK `:runtime` S-UPD-002..004 maintenance flow. The signed manifest is verified again here and
 * artifacts are read only from the deterministic update-cache path, never from a caller path.
 */
internal class UpdateMaintenanceController(
    private val packageName: String,
    private val config: ReleaseConfig,
    private val store: UpdateMaintenanceStore,
    private val host: MaintenanceHost,
    private val packages: InstalledPackageFacts,
    private val installer: ApkSessionInstaller,
    private val cacheRoot: File,
    private val clock: () -> Instant = Instant::now,
) {
    private val moduleId = if (packageName.endsWith(".debug")) "droidbridge_debug" else "droidbridge"

    fun state(): String = reply {
        val record = store.read()
        buildJsonObject {
            put("schema_version", 1)
            put("configured", config is ReleaseConfig.Configured)
            put("module", moduleFact().wire)
            put("privileged_install", host.privilegedInstallAvailable())
            put("installed_version_code", packages.installedVersionCode())
            put("record", record?.let { Json.parseToJsonElement(it.encode()) } ?: JsonNull)
        }.toString()
    }

    fun beginProductUpdate(manifestBytes: ByteArray, signature: ByteArray): String = reply {
        val configured = configured()
        val manifest = verified { ReleaseManifests.verify(configured, manifestBytes, signature) }
        if (manifest.versionCode <= packages.installedVersionCode()) refuse(INVALID_ARGUMENT, "release is not newer")
        val requiresModule = moduleFact() != ModuleObservation.Absent
        val apk = verifiedArtifact(manifest.version, manifest.apk)
        if (requiresModule) verifiedArtifact(manifest.version, manifest.module)
        val archive = packages.archive(apk) ?: refuse(INVALID_ARGUMENT, "APK cannot be parsed")
        if (archive.packageName != packageName || archive.versionCode != manifest.versionCode ||
            archive.signerSha256 != configured.apkSignerSha256
        ) {
            refuse(INVALID_ARGUMENT, "APK identity does not match the signed release")
        }
        val record = UpdateMaintenanceRecord(
            updateId = UpdateMaintenanceRecord.newUpdateId(),
            kind = MaintenanceKind.ProductUpdate,
            targetVersion = manifest.version,
            targetVersionCode = manifest.versionCode,
            targetApkSha256 = manifest.apk.sha256,
            targetApkSize = manifest.apk.size,
            targetApkSignerSha256 = configured.apkSignerSha256,
            targetModuleSha256 = manifest.module.sha256.takeIf { requiresModule },
            targetModuleSize = manifest.module.size.takeIf { requiresModule },
            maintenanceExecutionId = null,
            requiresModule = requiresModule,
            apkInstallProvider = if (host.privilegedInstallAvailable()) ApkInstallProvider.MagiskPrivileged else ApkInstallProvider.PackageInstaller,
            phase = MaintenancePhase.Prepared,
            apkSessionId = null,
        )
        enterMaintenance(record)
    }

    fun beginModuleRepair(manifestBytes: ByteArray, signature: ByteArray): String = reply {
        val configured = configured()
        val manifest = verified { ReleaseManifests.verify(configured, manifestBytes, signature) }
        if (manifest.versionCode != packages.installedVersionCode() ||
            packages.installedSignerSha256() != configured.apkSignerSha256
        ) {
            refuse(INVALID_ARGUMENT, "module repair must match the installed APK release")
        }
        if (moduleFact() == ModuleObservation.Compatible) refuse(INVALID_ARGUMENT, "the module is already compatible")
        verifiedArtifact(manifest.version, manifest.module)
        enterMaintenance(
            UpdateMaintenanceRecord(
                updateId = UpdateMaintenanceRecord.newUpdateId(),
                kind = MaintenanceKind.ModuleRepair,
                targetVersion = manifest.version,
                targetVersionCode = manifest.versionCode,
                targetApkSha256 = null,
                targetApkSize = null,
                targetApkSignerSha256 = configured.apkSignerSha256,
                targetModuleSha256 = manifest.module.sha256,
                targetModuleSize = manifest.module.size,
                maintenanceExecutionId = null,
                requiresModule = true,
                apkInstallProvider = null,
                phase = MaintenancePhase.ModulePending,
                apkSessionId = null,
            ),
        )
    }

    /** One explicit APK attempt from `prepared` through the recorded provider. */
    fun installApk(updateId: String): String = reply {
        val record = current(updateId)
        if (record.kind != MaintenanceKind.ProductUpdate || record.phase != MaintenancePhase.Prepared) {
            refuse(INVALID_ARGUMENT, "APK install is legal only from prepared")
        }
        val apk = cachedFile(record.targetVersion, apkName(record.targetVersion))
        if (!matches(apk, record.targetApkSize!!, record.targetApkSha256!!)) refuse(INVALID_ARGUMENT, "verified APK is no longer cached")
        when (record.apkInstallProvider) {
            ApkInstallProvider.PackageInstaller -> installWithPackageInstaller(record, apk)
            ApkInstallProvider.MagiskPrivileged -> installPrivilegedApk(record, apk)
            null -> refuse(INVALID_ARGUMENT, "product update has no APK provider")
        }
    }

    /** One explicit privileged module attempt from `module_pending`; success still waits for observation. */
    fun installModule(updateId: String): String = reply {
        val record = current(updateId)
        if (record.phase != MaintenancePhase.ModulePending) refuse(INVALID_ARGUMENT, "module install is legal only from module_pending")
        if (!host.privilegedInstallAvailable()) refuse(CAPABILITY_UNAVAILABLE, "privileged module install is unavailable")
        val module = cachedFile(record.targetVersion, moduleName(record.targetVersion))
        if (!matches(module, record.targetModuleSize!!, record.targetModuleSha256!!)) refuse(INVALID_ARGUMENT, "verified module is no longer cached")
        val installing = record.copy(phase = MaintenancePhase.ModuleInstalling, maintenanceExecutionId = UpdateMaintenanceRecord.newUpdateId())
        store.replace(record, installing)
        val outcome = host.privilegedInstall(PrivilegedArtifact.Module, installing, module)
        when {
            outcome.cleanupVerified && outcome.exitCode == 0 -> {
                // The module takes effect after Magisk reload; only a compatible observation completes it.
                val settled = installing.copy(maintenanceExecutionId = null)
                store.replace(installing, settled)
                success(settled)
            }
            outcome.cleanupVerified -> {
                store.replace(installing, record)
                refuse(IO_ERROR, "privileged module install failed")
            }
            else -> refuse(IO_ERROR, "privileged module install cleanup is unverified")
        }
    }

    fun cancel(updateId: String): String = reply {
        val record = current(updateId)
        if (record.maintenanceExecutionId != null) refuse(HOST_TRANSITION_PENDING, "the native attempt must be reconciled first")
        when (record.kind) {
            MaintenanceKind.ProductUpdate -> {
                if (record.phase != MaintenancePhase.Prepared && record.phase != MaintenancePhase.ApkInstalling) {
                    refuse(INVALID_ARGUMENT, "cancel is legal only before the APK is installed")
                }
                if (targetApkInstalled(record)) refuse(HOST_TRANSITION_PENDING, "the installed APK must be reconciled first")
                record.apkSessionId?.let(installer::abandon)
            }
            MaintenanceKind.ModuleRepair -> Unit
        }
        store.delete(record)
        reopen()
        success(null)
    }

    /** S-UPD-004 APK-only exit: exclusion commits before the record is removed. */
    fun continueWithoutModule(updateId: String): String = reply {
        val record = current(updateId)
        val modulePhase = record.phase == MaintenancePhase.ModulePending || record.phase == MaintenancePhase.ModuleInstalling
        if (!modulePhase || record.maintenanceExecutionId != null) refuse(INVALID_ARGUMENT, "APK-only exit needs a settled module step")
        if (!targetApkInstalled(record)) refuse(INVALID_ARGUMENT, "the target APK is not installed")
        if (!host.cleanupVerified()) refuse(IO_ERROR, "execution cleanup is unverified")
        store.excludeModuleAndFinish(
            record,
            ModuleExclusion(moduleId, record.updateId, clock().truncatedTo(ChronoUnit.SECONDS).toString()),
        )
        reopen()
        success(null)
    }

    /**
     * Observation-driven reconciliation after restart, package replacement, a module fact change or
     * an Updates read. It never starts an install and never infers success from callbacks.
     */
    fun recover() {
        val record = store.read() ?: return
        when (record.phase) {
            MaintenancePhase.Prepared -> installer.abandonUnrecordedSessions(null)
            MaintenancePhase.ApkInstalling -> when {
                targetApkInstalled(record) -> advanceAfterApk(record)
                record.apkInstallProvider == ApkInstallProvider.PackageInstaller -> {
                    installer.abandonUnrecordedSessions(record.apkSessionId)
                    val live = record.apkSessionId?.let(installer::exists) == true
                    if (!live) store.replace(record, record.copy(phase = MaintenancePhase.Prepared, apkSessionId = null))
                }
                // A privileged attempt returns to explicit Retry only after the daemon proves it clean.
                host.privilegedCleanup(record.updateId) == CLEAN ->
                    store.replace(record, record.copy(phase = MaintenancePhase.Prepared, maintenanceExecutionId = null))
                else -> Unit
            }
            MaintenancePhase.ApkInstalled -> advanceAfterApk(record)
            MaintenancePhase.ModulePending, MaintenancePhase.ModuleInstalling -> {
                val apkReady = record.kind == MaintenanceKind.ModuleRepair || targetApkInstalled(record)
                when {
                    apkReady && record.maintenanceExecutionId == null && host.moduleObservation() == ModuleObservation.Compatible -> {
                        store.removeExclusion()
                        store.delete(record)
                        reopen()
                    }
                    record.maintenanceExecutionId != null && host.privilegedCleanup(record.updateId) == CLEAN ->
                        store.replace(record, record.copy(phase = MaintenancePhase.ModulePending, maintenanceExecutionId = null))
                }
            }
        }
    }

    private fun installWithPackageInstaller(record: UpdateMaintenanceRecord, apk: File): String {
        installer.abandonUnrecordedSessions(null)
        val sessionId = installer.create(record.targetApkSize!!)
        val installing = record.copy(phase = MaintenancePhase.ApkInstalling, apkSessionId = sessionId)
        runCatching { store.replace(record, installing) }.onFailure { error ->
            installer.abandon(sessionId)
            throw error
        }
        try {
            installer.writeAndCommit(sessionId, apk, record.targetApkSize, record.targetApkSha256!!)
        } catch (error: Exception) {
            installer.abandon(sessionId)
            store.replace(installing, record)
            throw error
        }
        return success(installing)
    }

    private fun installPrivilegedApk(record: UpdateMaintenanceRecord, apk: File): String {
        if (!host.privilegedInstallAvailable()) refuse(CAPABILITY_UNAVAILABLE, "privileged install is unavailable")
        val installing = record.copy(phase = MaintenancePhase.ApkInstalling, maintenanceExecutionId = UpdateMaintenanceRecord.newUpdateId())
        store.replace(record, installing)
        val outcome = host.privilegedInstall(PrivilegedArtifact.Apk, installing, apk)
        // Replacing the package normally ends this process first; restart recovery observes it.
        if (targetApkInstalled(installing)) {
            advanceAfterApk(installing)
            return success(store.read())
        }
        if (!outcome.cleanupVerified) refuse(IO_ERROR, "privileged install cleanup is unverified")
        store.replace(installing, record)
        refuse(IO_ERROR, "privileged install did not install the target APK")
    }

    private fun advanceAfterApk(record: UpdateMaintenanceRecord) {
        if (record.requiresModule) {
            store.replace(record, record.copy(phase = MaintenancePhase.ModulePending, apkSessionId = null, maintenanceExecutionId = null))
            recover()
        } else {
            store.delete(record)
            reopen()
        }
    }

    private fun enterMaintenance(record: UpdateMaintenanceRecord): String {
        if (store.read() != null) refuse(HOST_TRANSITION_PENDING, "update maintenance is already recorded")
        host.ensureApkHost()?.let { refuse(it, "APK Runtime is not the active host") }
        host.closeAdmission()?.let { refuse(it, "business admission could not close") }
        try {
            store.create(record)
        } catch (error: Exception) {
            host.reopenAdmission()
            throw error
        }
        return success(record)
    }

    private fun reopen() {
        if (!host.reopenAdmission()) refuse(HOST_TRANSITION_PENDING, "business admission did not reopen")
    }

    private fun targetApkInstalled(record: UpdateMaintenanceRecord): Boolean =
        packages.installedVersionCode() == record.targetVersionCode &&
            packages.installedSignerSha256() == record.targetApkSignerSha256

    private fun moduleFact(): ModuleObservation =
        if (store.exclusionPresent()) ModuleObservation.Excluded else host.moduleObservation()

    private fun configured(): ReleaseConfig.Configured =
        config as? ReleaseConfig.Configured ?: refuse(CAPABILITY_UNAVAILABLE, "release configuration is unavailable")

    private fun current(updateId: String): UpdateMaintenanceRecord {
        val record = store.read() ?: refuse(STALE_AUTHORITY, "no update maintenance is recorded")
        if (record.updateId != updateId) refuse(STALE_AUTHORITY, "update maintenance identity changed")
        return record
    }

    private fun verifiedArtifact(version: String, artifact: ReleaseArtifact): File {
        val file = cachedFile(version, artifact.name)
        if (!matches(file, artifact.size, artifact.sha256)) refuse(INVALID_ARGUMENT, "${artifact.name} is not a verified download")
        return file
    }

    private fun apkName(version: String) = "droidbridge-$version-arm64-v8a.apk"

    private fun moduleName(version: String) = "droidbridge-magisk-$version.zip"

    private fun cachedFile(version: String, name: String) = File(File(cacheRoot, version), name)

    private fun matches(file: File, size: Long, sha256: String): Boolean =
        file.isFile && file.length() == size && ReleaseHash.sha256(file) == sha256

    private fun <T> verified(block: () -> T): T = try {
        block()
    } catch (rejected: ReleaseRejected) {
        refuse(INVALID_ARGUMENT, rejected.message.orEmpty())
    }

    private fun success(record: UpdateMaintenanceRecord?): String = buildJsonObject {
        put("schema_version", 1)
        put("record", record?.let { Json.parseToJsonElement(it.encode()) } ?: JsonNull)
    }.toString()

    private inline fun reply(block: () -> String): String = try {
        block()
    } catch (refused: MaintenanceRefused) {
        failure(refused.code)
    } catch (_: IllegalArgumentException) {
        failure(INVALID_ARGUMENT)
    } catch (_: IllegalStateException) {
        failure(STALE_AUTHORITY)
    } catch (_: Exception) {
        failure(IO_ERROR)
    }

    private fun failure(code: String): String = buildJsonObject {
        put("schema_version", 1)
        put("error", code)
    }.toString()

    private fun refuse(code: String, message: String): Nothing = throw MaintenanceRefused(code, message)

    private companion object {
        const val INVALID_ARGUMENT = "INVALID_ARGUMENT"
        const val CAPABILITY_UNAVAILABLE = "CAPABILITY_UNAVAILABLE"
        const val HOST_TRANSITION_PENDING = "HOST_TRANSITION_PENDING"
        const val STALE_AUTHORITY = "STALE_AUTHORITY"
        const val IO_ERROR = "IO_ERROR"
        const val CLEAN = "clean"
    }
}

internal class AndroidPackageFacts(private val context: Context) : InstalledPackageFacts {
    private val signingFlags = PackageManager.PackageInfoFlags.of(PackageManager.GET_SIGNING_CERTIFICATES.toLong())

    override fun installedVersionCode(): Long =
        context.packageManager.getPackageInfo(context.packageName, PackageManager.PackageInfoFlags.of(0)).longVersionCode

    override fun installedSignerSha256(): String? =
        context.packageManager.getPackageInfo(context.packageName, signingFlags).signingInfo?.let(::singleSigner)

    override fun archive(file: File): ArchiveFacts? =
        context.packageManager.getPackageArchiveInfo(file.absolutePath, signingFlags)?.let { info ->
            ArchiveFacts(info.packageName, info.longVersionCode, info.signingInfo?.let(::singleSigner))
        }

    private fun singleSigner(info: android.content.pm.SigningInfo): String? =
        info.takeUnless { it.hasMultipleSigners() }?.apkContentsSigners?.singleOrNull()?.let { signer ->
            MessageDigest.getInstance("SHA-256").digest(signer.toByteArray()).joinToString("") { "%02x".format(it) }
        }
}

internal class AndroidApkSessionInstaller(private val context: Context) : ApkSessionInstaller {
    private val installer = context.packageManager.packageInstaller

    override fun abandonUnrecordedSessions(recorded: Int?) {
        installer.mySessions
            .filter { it.appPackageName == context.packageName && it.sessionId != recorded }
            .forEach { installer.abandonSession(it.sessionId) }
    }

    override fun create(size: Long): Int = installer.createSession(
        PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setAppPackageName(context.packageName)
            setSize(size)
        },
    )

    override fun exists(sessionId: Int): Boolean = installer.getSessionInfo(sessionId) != null

    override fun writeAndCommit(sessionId: Int, apk: File, size: Long, sha256: String) {
        installer.openSession(sessionId).use { session ->
            val digest = MessageDigest.getInstance("SHA-256")
            var written = 0L
            apk.inputStream().use { input ->
                session.openWrite(BASE_APK, 0, size).use { output ->
                    val buffer = ByteArray(64 * 1024)
                    while (true) {
                        val read = input.read(buffer)
                        if (read < 0) break
                        digest.update(buffer, 0, read)
                        output.write(buffer, 0, read)
                        written += read
                    }
                    session.fsync(output)
                }
            }
            val actual = digest.digest().joinToString("") { "%02x".format(it) }
            check(written == size && actual == sha256) { "cached APK changed while it was staged" }
            // The platform installer adds its status extras, so this explicit broadcast must be mutable.
            val result = PendingIntent.getBroadcast(
                context,
                sessionId,
                Intent(context, PackageInstallerResultReceiver::class.java).setPackage(context.packageName),
                PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            session.commit(result.intentSender)
        }
    }

    override fun abandon(sessionId: Int) {
        if (installer.getSessionInfo(sessionId) != null) installer.abandonSession(sessionId)
    }

    private companion object {
        const val BASE_APK = "base.apk"
    }
}
