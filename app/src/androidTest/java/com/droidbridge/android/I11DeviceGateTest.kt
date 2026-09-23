package com.droidbridge.android

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.droidbridge.android.product.tasks.TaskRepository
import com.droidbridge.android.ui.tasks.TaskDetailRoute
import com.droidbridge.android.ui.tasks.TaskDetailViewModel
import java.util.concurrent.atomic.AtomicReference
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * I11 route contracts rendered by the real Compose/Material stack on the device. The same class runs
 * at the phone's own window and under the large-screen `wm size` fixture.
 */
@RunWith(AndroidJUnit4::class)
class I11DeviceGateTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun I11_G02_taskDetailCancelFollowsTheCanonicalSnapshot() {
        val cancelRequested = AtomicReference(false)
        val repository = TaskRepository(submit = { envelope ->
            val cancelling = envelope.decodeToString().contains("\"action\":\"cancel\"")
            if (cancelling) cancelRequested.set(true)
            """{"protocol_version":1,"request_id":"r","outcome":"success","result":{
                "task_id":"99700000-0000-4000-8000-000000000001","state":"running","tool":"command","action":"run",
                "created_at":"2026-09-15T08:00:00.000Z","started_at":"2026-09-15T08:00:01.000Z",
                "cancel_requested":${cancelRequested.get()},"execution_class":"app"}}""".encodeToByteArray()
        })
        val viewModel = TaskDetailViewModel(repository, "99700000-0000-4000-8000-000000000001")
        compose.setContent { MaterialTheme { TaskDetailRoute(viewModel) { } } }

        awaitTag("task_detail:cancel")
        compose.onNodeWithTag("task_detail:execution_class", useUnmergedTree = true).assertExists()
        compose.onNodeWithTag("task_detail:cancel").performClick()
        compose.waitUntil(TIMEOUT_MILLIS) {
            compose.onAllNodesWithTag("task_detail:cancel").fetchSemanticsNodes().isEmpty()
        }
        compose.onNodeWithTag("task_detail:state").assertExists()
    }

    private fun awaitTag(tag: String) {
        compose.waitUntil(TIMEOUT_MILLIS) { compose.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty() }
    }

    private companion object {
        const val TIMEOUT_MILLIS = 5_000L
    }
}
