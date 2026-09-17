package com.droidbridge.android.execution.android

internal data class AccessibilitySceneProof(
    val componentGeneration: Long,
    val windowId: Int,
    val sceneRevision: Long,
    val display: VisualDisplaySnapshot,
    val hierarchySha256: String,
)

internal class AccessibilitySceneStore<H>(
    private val capacity: Int,
    private val ttlMillis: Long,
    private val clockMillis: () -> Long,
    private val release: (H) -> Unit,
) {
    private data class Entry<H>(
        val createdAtMillis: Long,
        val proof: AccessibilitySceneProof,
        val handles: Map<String, H>,
    )

    private val entries = linkedMapOf<String, Entry<H>>()

    init {
        require(capacity > 0)
        require(ttlMillis > 0)
    }

    @Synchronized
    fun put(observationId: String, proof: AccessibilitySceneProof, handles: Map<String, H>) {
        evictExpired()
        entries.remove(observationId)?.release()
        while (entries.size >= capacity) {
            val oldest = entries.entries.first()
            entries.remove(oldest.key)
            oldest.value.release()
        }
        entries[observationId] = Entry(clockMillis(), proof, handles.toMap())
    }

    @Synchronized
    fun exact(observationId: String, nodeRef: String, proof: AccessibilitySceneProof): H? {
        evictExpired()
        val entry = entries[observationId] ?: return null
        if (entry.proof != proof) return null
        return entry.handles[nodeRef]
    }

    @Synchronized
    fun clear() {
        entries.values.forEach { it.release() }
        entries.clear()
    }

    @Synchronized
    fun expireAndNextDelayMillis(): Long? {
        val now = clockMillis()
        evictExpired(now)
        return entries.values.minOfOrNull { entry ->
            (ttlMillis - (now - entry.createdAtMillis)).coerceAtLeast(1L)
        }
    }

    private fun evictExpired(now: Long = clockMillis()) {
        val expired = entries.entries
            .filter { now - it.value.createdAtMillis >= ttlMillis }
            .map { it.key }
        expired.forEach { key -> entries.remove(key)?.release() }
    }

    private fun Entry<H>.release() {
        handles.values.forEach(release)
    }
}

internal class ProjectionSessionSlot<S> {
    private var generation = 0L
    private var session: S? = null

    @Synchronized
    fun publish(value: S): Long {
        check(session == null)
        check(generation < Long.MAX_VALUE)
        generation += 1
        session = value
        return generation
    }

    @Synchronized
    fun current(candidateGeneration: Long): S? =
        session.takeIf { candidateGeneration == generation }

    @Synchronized
    fun current(): S? = session

    @Synchronized
    fun take(): S? {
        val removed = session ?: return null
        session = null
        return removed
    }
}

internal fun decodeVisualRawRow(pixelFormat: Int, bytes: ByteArray): IntArray {
    val bytesPerPixel = when (pixelFormat) {
        1, 2, 5 -> 4
        3 -> 3
        4 -> 2
        else -> throw AndroidExecutionException("IO_ERROR")
    }
    if (bytes.isEmpty() || bytes.size % bytesPerPixel != 0) {
        throw AndroidExecutionException("IO_ERROR")
    }
    return IntArray(bytes.size / bytesPerPixel) { index ->
        val offset = index * bytesPerPixel
        when (pixelFormat) {
            1 -> argb(bytes[offset + 3], bytes[offset], bytes[offset + 1], bytes[offset + 2])
            2 -> argb(0xff.toByte(), bytes[offset], bytes[offset + 1], bytes[offset + 2])
            3 -> argb(0xff.toByte(), bytes[offset], bytes[offset + 1], bytes[offset + 2])
            4 -> {
                val packed = unsigned(bytes[offset]) or (unsigned(bytes[offset + 1]) shl 8)
                val red5 = (packed ushr 11) and 0x1f
                val green6 = (packed ushr 5) and 0x3f
                val blue5 = packed and 0x1f
                argb(
                    0xff.toByte(),
                    ((red5 shl 3) or (red5 ushr 2)).toByte(),
                    ((green6 shl 2) or (green6 ushr 4)).toByte(),
                    ((blue5 shl 3) or (blue5 ushr 2)).toByte(),
                )
            }
            5 -> argb(bytes[offset + 3], bytes[offset + 2], bytes[offset + 1], bytes[offset])
            else -> error("validated pixel format")
        }
    }
}

private fun argb(alpha: Byte, red: Byte, green: Byte, blue: Byte): Int =
    (unsigned(alpha) shl 24) or
        (unsigned(red) shl 16) or
        (unsigned(green) shl 8) or
        unsigned(blue)

private fun unsigned(value: Byte): Int = value.toInt() and 0xff
