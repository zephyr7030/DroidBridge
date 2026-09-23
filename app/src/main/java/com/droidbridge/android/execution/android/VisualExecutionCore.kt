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
    fun matches(observationId: String, proof: AccessibilitySceneProof): Boolean {
        evictExpired()
        return entries[observationId]?.proof == proof
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
