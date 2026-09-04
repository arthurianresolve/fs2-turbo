#requires -Version 7.0
param([Parameter(Mandatory)][string]$EvidenceRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$source = Join-Path $PSScriptRoot '../windows-benchmark-pilot.ps1'
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($source, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
$common = @($ast.FindAll({
    param($node)
    $node -is [Management.Automation.Language.StringConstantExpressionAst] -and
        $node.Value.Contains('function Resolve-PilotApplication(')
}, $true))
if ($common.Count -ne 1) { throw 'Expected exactly one shared launcher implementation.' }
. ([scriptblock]::Create($common[0].Value))
$token = [Security.Principal.WindowsIdentity]::GetCurrent()
if ([Security.Principal.WindowsPrincipal]::new($token).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run launch regression checks as a standard user.'
}
$EvidenceRoot = [IO.Path]::GetFullPath($EvidenceRoot)
New-PilotRoot $EvidenceRoot $token.User
$token.Dispose()
$Audit = Join-Path $EvidenceRoot 'audit'
[IO.Directory]::CreateDirectory($Audit) | Out-Null
$pwsh = (Get-Process -Id $PID).Path
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
    Check 'storage-uses-system-volume-not-checkout-volume' {
        $text = $ast.Extent.Text
        $start = $text.IndexOf('$drive =')
        $end = $text.IndexOf('$common =')
        if ($start -lt 0 -or $end -le $start) { throw 'Storage setup block is missing.' }
        $select = [scriptblock]::Create($text.Substring($start, $end - $start) +
            [Environment]::NewLine + '[pscustomobject]@{drive=$drive;root=$root}')
        $names = @('GITHUB_WORKSPACE', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT')
        $saved = @{}
        foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
        try {
            $env:GITHUB_WORKSPACE = 'Z:\unrelated-checkout'
            $env:GITHUB_RUN_ID = [DateTime]::UtcNow.Ticks.ToString()
            $env:GITHUB_RUN_ATTEMPT = '1'
            $selection = & $select
            if ($selection.drive -cne [IO.Path]::GetPathRoot($env:SystemRoot) -or
                -not $selection.root.StartsWith($selection.drive, [StringComparison]::OrdinalIgnoreCase)) {
                throw 'Pilot storage still follows the checkout volume.'
            }
        } finally {
            foreach ($name in $names) {
                if ($null -eq $saved[$name]) { Remove-Item -LiteralPath ('Env:' + $name) -ErrorAction SilentlyContinue }
                else { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
            }
        }
    }
    Check 'multiple-path-results-reproduce-invalid-string-binding' {
        $first = Join-Path $EvidenceRoot 'first tools'
        $second = Join-Path $EvidenceRoot 'second tools'
        foreach ($directory in @($first, $second)) {
            [IO.Directory]::CreateDirectory($directory) | Out-Null
            # Resolver-only sentinels are deliberately never executed.
            [IO.File]::WriteAllText((Join-Path $directory 'fs2-native-probe.exe'), 'resolver-only sentinel')
        }
        $originalPath = $env:PATH
        try {
            $env:PATH = $first + ';' + $second + ';' + $originalPath
            $commands = @(Get-Command fs2-native-probe.exe -CommandType Application)
            if ($commands.Count -ne 2) { throw 'Duplicate PATH fixture did not bind.' }
            $invalid = [string]$commands.Source
            if ([IO.File]::Exists($invalid)) { throw 'Expected the old combined path to be invalid.' }
            $resolved = @(Resolve-PilotApplication 'fs2-native-probe.exe')
            if ($resolved.Count -ne 1 -or $resolved[0] -cne (Join-Path $first 'fs2-native-probe.exe')) {
                throw 'Resolver did not select exactly the first PATH application.'
            }
        } finally { $env:PATH = $originalPath }
    }
    Check 'missing-application-refused' {
        Must-Reject { Resolve-PilotApplication 'fs2-no-such-application-91fd7c.exe' | Out-Null }
    }
    Check 'absolute-executable-native-zero-exit' {
        $receipt = Invoke-Native $pwsh @('-NoProfile', '-NonInteractive', '-Command', 'exit 0') 'zero'
        if ($receipt.exit -ne 0 -or $receipt.executable -cne $pwsh) { throw 'Native success receipt mismatch.' }
    }
    Check 'native-nonzero-exit-retained' {
        $receipt = Invoke-Native $pwsh @('-NoProfile', '-NonInteractive', '-Command', 'exit 7') 'nonzero' -AllowFailure
        if ($receipt.exit -ne 7) { throw 'Native nonzero exit was lost.' }
    }
    Check 'unexpected-native-failure-refused' {
        Must-Reject { Invoke-Native $pwsh @('-NoProfile', '-NonInteractive', '-Command', 'exit 7') 'refused' | Out-Null }
    }
    Check 'space-and-trailing-backslash-argument-preserved' {
        $probe = Join-Path $EvidenceRoot 'argument probe.ps1'
        [IO.File]::WriteAllText($probe, 'param([string]$Value); [Console]::WriteLine($Value)', [Text.UTF8Encoding]::new($false))
        $value = 'C:\fixture path with spaces\'
        $receipt = Invoke-Native $pwsh @('-NoProfile', '-NonInteractive', '-File', $probe, '-Value', $value) 'argument'
        $actual = [IO.File]::ReadAllText($receipt.stdout).TrimEnd([char[]]@([char]13, [char]10))
        if ($actual -cne $value) { throw 'Native argument spelling changed.' }
    }
    Check 'invalid-executable-retains-diagnostic-receipt' {
        $missing = Join-Path $EvidenceRoot 'missing executable.exe'
        Must-Reject { Invoke-Native $missing @('--version') 'missing' | Out-Null }
        $receipt = Get-Content -LiteralPath (Join-Path $Audit '005-missing.receipt.json') -Raw | ConvertFrom-Json
        if ($receipt.executable -cne $missing -or $null -ne $receipt.exit -or
            -not $receipt.failure.Contains($missing)) {
            throw 'Missing executable diagnostic was not retained.'
        }
    }
} catch {
    $failure = $_.Exception.Message
} finally {
    Write-NewJson (Join-Path $EvidenceRoot 'outcome.json') @{
        passed = ($null -eq $failure); checks = @($checks.ToArray()); failure = $failure
        scope = 'native standard-user executable resolution and launch mechanics; no benchmark measurements'
        source_sha256 = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
        finished_utc = [DateTime]::UtcNow.ToString('o')
    }
}
$checks.ToArray() | ConvertTo-Json -Depth 4
if ($null -ne $failure) { Write-Error $failure -ErrorAction Continue; exit 1 }
exit 0
