package com.droidbridge.ui

import com.droidbridge.ui.product.mcp.McpSetupPrompt
import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

class I11_McpSetupPromptTest {
    /** The shipped template of one resource directory, with Android's string escapes resolved. */
    private fun template(directory: String): String {
        val strings = DocumentBuilderFactory.newInstance().newDocumentBuilder()
            .parse(File("src/main/res/$directory/strings.xml"))
            .getElementsByTagName("string")
        val raw = (0 until strings.length).map { strings.item(it) as Element }
            .single { it.getAttribute("name") == "mcp_setup_prompt_template" }
            .textContent
        return raw.replace("\\n", "\n").replace("\\'", "'").replace("\\\"", "\"")
    }

    @Test
    fun I11_setupPromptCarriesTheEndpointItsPortAndTheTokenInEveryLocale() {
        for (directory in listOf("values", "values-zh-rCN")) {
            val prompt = McpSetupPrompt.compose(template(directory), "http://127.0.0.1:18766/mcp", "tok-123")
            assertNotNull(directory, prompt)
            prompt!!
            assertTrue(directory, prompt.contains("http://127.0.0.1:18766/mcp"))
            assertTrue(directory, prompt.contains("adb forward tcp:18766 tcp:18766"))
            assertTrue(directory, prompt.contains("Authorization: Bearer tok-123"))
            assertTrue(directory, prompt.contains("droidbridge"))
            assertTrue(directory, prompt.contains("""{"action": "status", "input": {}}"""))
            assertFalse(directory, prompt.contains("%"))
        }
    }

    /** AGENT-SETUP.md publishes the same prompt with placeholders; the two must not drift. */
    @Test
    fun I11_repositoryPromptMatchesTheShippedTemplates() {
        val document = File("../AGENT-SETUP.md").readText().replace("\r\n", "\n")
        val blocks = Regex("```text\n(.*?)\n```", RegexOption.DOT_MATCHES_ALL).findAll(document)
            .map { it.groupValues[1] }.toList()
        assertTrue("one English and one Chinese block", blocks.size == 2)
        for ((directory, block) in listOf("values", "values-zh-rCN").zip(blocks)) {
            val shipped = McpSetupPrompt.compose(template(directory), "http://127.0.0.1:18766/mcp", "tok-123")
            assertTrue(
                directory,
                shipped == block.replace("<PORT>", "18766").replace("<TOKEN>", "tok-123"),
            )
        }
    }

    @Test
    fun I11_setupPromptRefusesAnEndpointWithoutAPortAndAnEmptyToken() {
        val template = "%1\$s %2\$s %3\$s"
        assertNull(McpSetupPrompt.compose(template, "http://127.0.0.1/mcp", "tok"))
        assertNull(McpSetupPrompt.compose(template, "not a uri", "tok"))
        assertNull(McpSetupPrompt.compose(template, "http://127.0.0.1:8766/mcp", ""))
    }
}
