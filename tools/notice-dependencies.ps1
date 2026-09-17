function Assert-NoticeDependencies($Metadata, [object[]]$Rows) {
    foreach ($member in $Metadata.workspace_members) {
        $node = @($Metadata.resolve.nodes | Where-Object id -eq $member)
        if ($node.Count -ne 1) { throw "workspace dependency resolution missing: $member" }
        foreach ($id in $node[0].dependencies) {
            $packages = @($Metadata.packages | Where-Object id -eq $id)
            if ($packages.Count -ne 1) { throw "resolved dependency package missing: $id" }
            $package = $packages[0]
            if ($Metadata.workspace_members -contains $id) { continue }
            $approved = @($Rows | Where-Object {
                $_.kind -eq 'rust' -and $_.name -eq $package.name -and $_.version -eq $package.version
            })
            if ($approved.Count -ne 1) {
                throw "locked Rust dependency inventory missing/mismatched: $($package.name) $($package.version)"
            }
        }
    }
}
