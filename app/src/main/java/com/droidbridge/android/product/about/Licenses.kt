package com.droidbridge.android.product.about

data class LicenseEntry(val name: String, val version: String, val license: String) {
    val supportingText: String get() = "$version | $license"
}

/** S-UI-017 About data, read from the packaged release provenance inventory. */
object ProductInfo {
    const val INVENTORY_ASSET = "third-party-direct.tsv"
    const val NOTICES_ASSET = "THIRD_PARTY_NOTICES.txt"
    private const val HEADER = "kind\tname\tversion\tlicense\tsource"
    private const val BUILD_TOOL = "build-tool"
    private const val UNCONFIGURED = "UNCONFIGURED"

    /** Every packaged row, excluding build-only tools, sorted case-insensitively by name. */
    fun licenses(inventory: String): List<LicenseEntry> {
        val lines = inventory.lines().filter(String::isNotEmpty)
        require(lines.firstOrNull() == HEADER) { "unexpected third-party inventory header" }
        return lines.drop(1)
            .map { line ->
                val fields = line.split('\t')
                require(fields.size == 5 && fields.none(String::isBlank)) { "malformed third-party row" }
                fields
            }
            .filter { fields -> fields[0] != BUILD_TOOL }
            .map { fields -> LicenseEntry(name = fields[1], version = fields[2], license = fields[3]) }
            .sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER, LicenseEntry::name))
    }

    /** The repository row exists exactly when both release values are configured. */
    fun repositoryUrl(owner: String, repository: String): String? =
        if (owner.isBlank() || repository.isBlank() || owner == UNCONFIGURED || repository == UNCONFIGURED) {
            null
        } else {
            "https://github.com/$owner/$repository"
        }
}
