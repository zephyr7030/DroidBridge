$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot/notice-dependencies.ps1"
$assertions = 0

function Check([string]$label, [object[]]$rows, [string]$version, [string]$failure) {
    $metadata = @{
        workspace_members = @('local')
        packages = @(
            @{id='local';name='contract';version='0.1.0';source=$null},
            @{id='direct';name='serde';version=$version;source='registry'},
            @{id='transitive';name='serde_core';version='1.0.229';source='registry'}
        )
        resolve = @{nodes=@(
            @{id='local';dependencies=@('direct')},
            @{id='direct';dependencies=@('transitive')}
        )}
    } | ConvertTo-Json -Depth 10 | ConvertFrom-Json
    $caught = $null
    try { Assert-NoticeDependencies $metadata $rows } catch { $caught = $_.Exception.Message }
    if ($failure) {
        if (-not $caught -or -not $caught.Contains($failure)) { throw "${label}: expected '$failure', got '$caught'" }
    } elseif ($caught) { throw "${label}: unexpected failure: $caught" }
    $script:assertions++
    Write-Output "PASS $label"
}

$approved = [pscustomobject]@{kind='rust';name='serde';version='1.0.229'}
$future = [pscustomobject]@{kind='rust';name='base64';version='0.23.1'}
Check 'materialized-direct-dependency' @($approved) '1.0.229' ''
Check 'unmaterialized-approved-dependency' @($approved,$future) '1.0.229' ''
Check 'missing-inventory-rejected' @($future) '1.0.229' 'missing/mismatched'
Check 'wrong-version-rejected' @($approved) '1.0.228' 'missing/mismatched'
Check 'non-rust-row-rejected' @([pscustomobject]@{kind='gradle';name='serde';version='1.0.229'}) '1.0.229' 'missing/mismatched'
Write-Output "I0R_NOTICE_ASSERTIONS=$assertions"
