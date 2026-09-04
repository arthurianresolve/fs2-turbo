#requires -Version 7.0
param([Parameter(Mandatory)][string]$EvidenceRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$token = [Security.Principal.WindowsIdentity]::GetCurrent()
if ([Security.Principal.WindowsPrincipal]::new($token).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run ownership regression checks as a standard user.'
}
$source = Join-Path $PSScriptRoot '../windows-benchmark-pilot.ps1'
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($source, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
$common = @($ast.FindAll({
    param($node)
    $node -is [Management.Automation.Language.StringConstantExpressionAst] -and
        $node.Value.Contains('function New-PilotRoot(')
}, $true))
if ($common.Count -ne 1) { throw 'Expected exactly one shared launcher implementation.' }
. ([scriptblock]::Create($common[0].Value))
$EvidenceRoot = [IO.Path]::GetFullPath($EvidenceRoot)
New-PilotRoot $EvidenceRoot $token.User
$checks = [Collections.Generic.List[object]]::new()
$failure = $null
function Check([string]$Name, [scriptblock]$Body) {
    try {
        & $Body
        $checks.Add(@{ name = $Name; passed = $true })
    } catch {
        $checks.Add(@{ name = $Name; passed = $false; error = $_.Exception.Message })
        throw
    }
}
function Must-Reject([scriptblock]$Body) {
    $rejected = $false
    try { & $Body } catch { $rejected = $true }
    if (-not $rejected) { throw 'Expected refusal.' }
}
try {
    Check 'foreign-owner-create-reproduces-1307' {
        $acl = [Security.AccessControl.DirectorySecurity]::new()
        $acl.SetOwner([Security.Principal.SecurityIdentifier]::new('S-1-5-18'))
        $foreign = Join-Path $EvidenceRoot 'foreign-owner'
        $code = $null
        try { [IO.FileSystemAclExtensions]::Create([IO.DirectoryInfo]::new($foreign), $acl) }
        catch { $code = $_.Exception.GetBaseException().HResult -band 0xffff }
        if ($code -ne 1307) { throw ('Expected ERROR_INVALID_OWNER 1307, got ' + $code) }
    }
    Check 'current-owner-private-create' {
        $null = Assert-PilotRoot $EvidenceRoot $token.User.Value $token.User.Value
    }
    Check 'standard-user-owner-only-handoff-preserves-dacl' {
        Set-PilotWorkerOwnership $EvidenceRoot $token.User.Value $token.User.Value
    }
    Check 'reused-root-refused' {
        Must-Reject { New-PilotRoot $EvidenceRoot $token.User }
    }
    Check 'wrong-provisioner-refused' {
        Must-Reject { Set-PilotWorkerOwnership $EvidenceRoot 'S-1-5-18' $token.User.Value }
    }
    Check 'wrong-worker-refused' {
        Must-Reject { Set-PilotWorkerOwnership $EvidenceRoot $token.User.Value 'S-1-5-18' }
    }
    Check 'non-directory-refused' {
        $file = Join-Path $EvidenceRoot 'regular-file'
        [IO.File]::WriteAllText($file, 'not a directory')
        Must-Reject { Assert-PilotRoot $file $token.User.Value $token.User.Value | Out-Null }
    }
    Check 'missing-system-ace-refused' {
        $path = Join-Path $EvidenceRoot 'missing-system'
        New-PilotRoot $path $token.User
        $acl = Get-Acl -LiteralPath $path
        foreach ($ace in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
            if ($ace.IdentityReference.Value -ceq 'S-1-5-18') { $acl.RemoveAccessRuleSpecific($ace) }
        }
        [IO.FileSystemAclExtensions]::SetAccessControl([IO.DirectoryInfo]::new($path), $acl)
        Must-Reject { Set-PilotWorkerOwnership $path $token.User.Value $token.User.Value }
    }
    Check 'unprotected-dacl-refused' {
        $path = Join-Path $EvidenceRoot 'unprotected'
        New-PilotRoot $path $token.User
        $acl = Get-Acl -LiteralPath $path
        $acl.SetAccessRuleProtection($false, $false)
        [IO.FileSystemAclExtensions]::SetAccessControl([IO.DirectoryInfo]::new($path), $acl)
        Must-Reject { Set-PilotWorkerOwnership $path $token.User.Value $token.User.Value }
    }
} catch {
    $failure = $_.Exception.Message
} finally {
    Write-NewJson (Join-Path $EvidenceRoot 'outcome.json') @{
        passed = ($null -eq $failure); checks = @($checks.ToArray()); failure = $failure
        scope = 'native standard-user ownership mechanics; hosted run verifies the distinct provisioner-to-worker transition'
        user_sid = $token.User.Value; elevated = $false
        source_sha256 = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
        finished_utc = [DateTime]::UtcNow.ToString('o')
    }
    $token.Dispose()
}
$checks.ToArray() | ConvertTo-Json -Depth 4
if ($null -ne $failure) { Write-Error $failure -ErrorAction Continue; exit 1 }
exit 0
