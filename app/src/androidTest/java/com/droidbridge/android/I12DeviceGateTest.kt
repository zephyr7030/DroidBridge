package com.droidbridge.android

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.droidbridge.android.runtimehost.AndroidApkSessionInstaller
import com.droidbridge.android.runtimehost.AndroidPackageFacts
import java.io.File
import java.security.MessageDigest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * I12 platform adapters on the real device. The session is written and abandoned but never
 * committed, because committing a self-update would replace the package running this test.
 */
@RunWith(AndroidJUnit4::class)
class I12DeviceGateTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun I12_G03_packageFactsReadTheInstalledAndArchiveSigner() {
        val facts = AndroidPackageFacts(context)
        val installedSigner = facts.installedSignerSha256()
        assertNotNull(installedSigner)
        val archive = facts.archive(File(context.applicationInfo.sourceDir))
        assertNotNull(archive)
        assertEquals(context.packageName, archive!!.packageName)
        assertEquals(facts.installedVersionCode(), archive.versionCode)
        assertEquals(installedSigner, archive.signerSha256)
    }

    @Test
    fun I12_G03_selfUpdateSessionIsCreatedWrittenAndAbandonedWithoutCommit() {
        val installer = AndroidApkSessionInstaller(context)
        val apk = File(context.applicationInfo.sourceDir)
        installer.abandonUnrecordedSessions(null)
        val sessionId = installer.create(apk.length())
        try {
            assertTrue(installer.exists(sessionId))
            val session = context.packageManager.packageInstaller.getSessionInfo(sessionId)!!
            assertEquals(context.packageName, session.appPackageName)
            // Writing without commit proves the byte path; the leftover session is abandoned below.
            context.packageManager.packageInstaller.openSession(sessionId).use { opened ->
                opened.openWrite("base.apk", 0, apk.length()).use { output ->
                    apk.inputStream().use { it.copyTo(output) }
                    opened.fsync(output)
                }
            }
            installer.abandonUnrecordedSessions(sessionId)
            assertTrue(installer.exists(sessionId))
        } finally {
            installer.abandon(sessionId)
        }
        // PackageInstaller removes an abandoned session asynchronously on its install thread.
        val deadline = System.currentTimeMillis() + 5_000
        while (installer.exists(sessionId) && System.currentTimeMillis() < deadline) Thread.sleep(50)
        assertFalse(installer.exists(sessionId))
        assertTrue(sha256(apk).length == 64)
    }

    private fun sha256(file: File) = MessageDigest.getInstance("SHA-256").digest(file.readBytes()).joinToString("") { "%02x".format(it) }
}
