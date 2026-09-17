package com.droidbridge.android

import com.droidbridge.android.execution.android.AndroidExecutionBridge
import com.droidbridge.android.execution.android.AndroidExecutionException
import com.droidbridge.android.execution.android.AndroidExecutionRegistry
import com.droidbridge.android.execution.android.AndroidExecutionResult
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.CapabilityRegistration
import com.droidbridge.android.execution.android.ContentInspection
import com.droidbridge.android.execution.android.ContentResolverAccess
import com.droidbridge.android.execution.android.ContentResolverFilesystemAdapter
import com.droidbridge.android.execution.android.NativeAndroidExecutionDispatcher
import com.droidbridge.android.execution.android.RegisteredCapabilityState
import com.droidbridge.android.execution.android.formatContentModifiedAt
import com.droidbridge.android.execution.shizuku.ShizukuExecutionException
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

class I8_FsAndroidAdapterTest {
    @Test
    fun I8_FS_G01_contentInspectRevalidatesFenceAndReturnsTheExactPublicShape() = runBlocking {
        var observedUri: String? = null
        val access = object : ContentResolverAccess {
            override fun inspect(
                uri: String,
                recursive: Boolean,
                maxDepth: Int,
                maxEntries: Int,
            ): ContentInspection {
                observedUri = uri
                return ContentInspection("file", 6, null, null, null)
            }

            override fun openRead(uri: String) = throw AssertionError("unexpected")
        }
       
        val adapter = ContentResolverFilesystemAdapter(access) { epoch, generation, instance ->
            epoch == "10000000-0000-4000-8000-000000000001" &&
                generation == 4L &&
                instance == "10000000-0000-4000-8000-000000000002"
        }
        val result = adapter.execute(
            com.droidbridge.android.execution.android.AndroidExecutionRequest(
                primitive = AndroidPrimitive.ContentInspect,
                payload = """{"target":{"type":"content_uri","value":"content://authority/document/1"},"recursive":false,"max_depth":1,"max_entries":200}""".encodeToByteArray(),
                executionId = "10000000-0000-4000-8000-000000000003",
                runtimeEpoch = "10000000-0000-4000-8000-000000000001",
                hostGeneration = 4,
                runtimeInstanceId = "10000000-0000-4000-8000-000000000002",
            ),
        )

        assertEquals("content://authority/document/1", observedUri)
        val json = Json.parseToJsonElement(result.payload.decodeToString()).jsonObject
        assertEquals("content_uri", json.getValue("target").jsonObject.getValue("type").jsonPrimitive.content)
        assertEquals("content://authority/document/1", json.getValue("target").jsonObject.getValue("value").jsonPrimitive.content)
        assertEquals("file", json.getValue("type").jsonPrimitive.content)
        assertEquals("6", json.getValue("size").jsonPrimitive.content)
        assertEquals(setOf("target", "type", "size"), json.keys)
    }

    @Test
    fun I8_FS_G04_nativeDispatcherUsesTheExactRegisteredGeneration() {
        val registry = AndroidExecutionRegistry { _, _, _, _, _ -> true }
        var observedPrimitive: AndroidPrimitive? = null
        val executor = AndroidExecutionBridge { request ->
            observedPrimitive = request.primitive
            AndroidExecutionResult(request.payload.reversedArray())
        }
        assertTrue(
            registry.register(
                CapabilityRegistration(
                    key = "android.framework",
                    state = RegisteredCapabilityState.Available,
                    reason = null,
                    sourceGeneration = 7,
                    executor = executor,
                ),
            ),
        )
        NativeAndroidExecutionDispatcher.install(registry)
        try {
            assertNull(
                NativeAndroidExecutionDispatcher.execute(
                    "android.framework",
                    6,
                    "ContentInspect",
                    byteArrayOf(1, 2, 3),
                    "00000000-0000-4000-8000-000000000001",
                    "00000000-0000-4000-8000-000000000002",
                    4,
                    "00000000-0000-4000-8000-000000000003",
                ),
            )
            val result = requireNotNull(
                NativeAndroidExecutionDispatcher.execute(
                    "android.framework",
                    7,
                    "ContentInspect",
                    byteArrayOf(1, 2, 3),
                    "00000000-0000-4000-8000-000000000001",
                    "00000000-0000-4000-8000-000000000002",
                    4,
                    "00000000-0000-4000-8000-000000000003",
                ),
            )
            assertEquals(AndroidPrimitive.ContentInspect, observedPrimitive)
            assertArrayEquals(byteArrayOf(3, 2, 1), result.payload)

            assertTrue(
                registry.register(
                    CapabilityRegistration(
                        key = "android.framework",
                        state = RegisteredCapabilityState.Available,
                        reason = null,
                        sourceGeneration = 8,
                        executor = AndroidExecutionBridge {
                            throw ShizukuExecutionException("NOT_FOUND")
                        },
                    ),
                ),
            )
            val failed = requireNotNull(
                NativeAndroidExecutionDispatcher.execute(
                    "android.framework",
                    8,
                    "ContentInspect",
                    byteArrayOf(),
                    "00000000-0000-4000-8000-000000000001",
                    "00000000-0000-4000-8000-000000000002",
                    4,
                    "00000000-0000-4000-8000-000000000003",
                ),
            )
            val sharedError: AndroidExecutionException = ShizukuExecutionException("NOT_FOUND")
            assertEquals("NOT_FOUND", sharedError.code)
            assertEquals("NOT_FOUND", failed.errorCode)
        } finally {
            NativeAndroidExecutionDispatcher.uninstall(registry)
        }
    }

    @Test
    fun I8_FS_G01_contentTimestampsUseExactUtcMillisecondPrecision() {
        assertEquals("2026-09-12T00:00:00.000Z", formatContentModifiedAt(1_789_171_200_000))
        assertEquals("2026-09-12T00:00:00.042Z", formatContentModifiedAt(1_789_171_200_042))
    }
}
