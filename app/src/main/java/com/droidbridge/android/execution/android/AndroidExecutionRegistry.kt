package com.droidbridge.android.execution.android

enum class RegisteredCapabilityState(val wireValue: String) {
    Available("available"),
    Unavailable("unavailable"),
    Unknown("unknown"),
}

data class CapabilityRegistration(
    val key: String,
    val state: RegisteredCapabilityState,
    val reason: String?,
    val sourceGeneration: Long,
    val executor: AndroidExecutionBridge? = null,
    val primitives: Set<AndroidPrimitive> = emptySet(),
)

internal class AndroidExecutionRegistry(
    private val registrationSink: (
        key: String,
        state: String,
        reason: String,
        generation: Long,
        hasExecutor: Boolean,
    ) -> Boolean,
) {
    private data class Entry(
        val generation: Long,
        val executor: AndroidExecutionBridge?,
        val primitives: Set<AndroidPrimitive> = emptySet(),
    )

    private val entries = mutableMapOf<String, Entry>()
    private val primitiveEntries = mutableMapOf<AndroidPrimitive, MutableMap<Long, Entry>>()

    @Synchronized
    fun register(registration: CapabilityRegistration): Boolean {
        require(registration.sourceGeneration > 0)
        require(
            registration.state == RegisteredCapabilityState.Available ||
                (registration.executor == null && registration.primitives.isEmpty()),
        )
        require(registration.primitives.isEmpty() || registration.executor != null)
        val current = entries[registration.key]
        if (current != null && registration.sourceGeneration < current.generation) return false
        registration.primitives.forEach { primitive ->
            val existing = primitiveEntries[primitive]?.get(registration.sourceGeneration)
            require(existing == null || existing.executor === registration.executor)
        }
        val accepted = registrationSink(
            registration.key,
            registration.state.wireValue,
            registration.reason.orEmpty(),
            registration.sourceGeneration,
            registration.executor != null,
        )
        if (accepted) {
            current?.primitives?.forEach { primitive ->
                primitiveEntries[primitive]?.let { generations ->
                    generations.remove(current.generation)
                    if (generations.isEmpty()) primitiveEntries.remove(primitive)
                }
            }
            val entry = Entry(
                registration.sourceGeneration,
                registration.executor,
                registration.primitives,
            )
            entries[registration.key] = entry
            registration.primitives.forEach { primitive ->
                val generations = primitiveEntries.getOrPut(primitive, ::mutableMapOf)
                generations[entry.generation] = entry
            }
        }
        return accepted
    }

    @Synchronized
    fun executor(key: String, generation: Long): AndroidExecutionBridge? {
        val entry = entries[key] ?: return null
        return entry.executor.takeIf { entry.generation == generation }
    }

    @Synchronized
    fun executor(primitive: AndroidPrimitive, generation: Long): AndroidExecutionBridge? {
        val entry = primitiveEntries[primitive]?.get(generation) ?: return null
        return entry.executor.takeIf { entry.generation == generation }
    }
}
