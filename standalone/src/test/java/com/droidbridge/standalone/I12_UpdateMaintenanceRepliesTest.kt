package com.droidbridge.standalone

import com.droidbridge.standalone.product.update.MaintenanceReply
import com.droidbridge.standalone.product.update.UpdateMaintenanceReplies
import org.junit.Assert.assertEquals
import org.junit.Test

class I12_UpdateMaintenanceRepliesTest {
    @Test
    fun refusalKeepsTheCodeAndStageAndAcceptsLegacyReplies() {
        assertEquals(
            MaintenanceReply.Refused("HOST_TRANSITION_PENDING", "close_admission"),
            UpdateMaintenanceReplies.mutation("""{"schema_version":1,"error":"HOST_TRANSITION_PENDING","stage":"close_admission"}"""),
        )
        assertEquals(
            MaintenanceReply.Refused("IO_ERROR"),
            UpdateMaintenanceReplies.mutation("""{"schema_version":1,"error":"IO_ERROR"}"""),
        )
    }

    @Test
    fun malformedOrUnboundedResponsesCannotBecomeRuntimeRefusals() {
        val invalid = listOf(
            """{"schema_version":2,"error":"IO_ERROR"}""",
            """{"schema_version":1,"error":123}""",
            """{"schema_version":1,"error":"IO_ERROR","stage":true}""",
            """{"schema_version":1,"error":"IO_ERROR","record":null}""",
            """{"schema_version":1,"error":"private/path"}""",
            """{"schema_version":1,"error":"""" + "A".repeat(65) + """"}""",
        )
        invalid.forEach {
            assertEquals(MaintenanceReply.Refused("RESPONSE_INVALID", "response_decode"), UpdateMaintenanceReplies.mutation(it))
        }
    }
}
