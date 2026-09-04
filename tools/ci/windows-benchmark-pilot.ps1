#requires -Version 7.0
param(
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$CandidateSha,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$WorkflowSha
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# This is VM provisioning and process launch only. Workloads, admission,
# containment, statistics and acceptance remain owned by fs2-dev.
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:GITHUB_EVENT_NAME -notin @('push', 'workflow_dispatch') -or
    $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:RUNNER_OS -ne 'Windows' -or
    $env:GITHUB_REPOSITORY -ne 'arthurianresolve/fs2-turbo') {
    throw 'This launcher is restricted to the trusted repository on a GitHub-hosted Windows VM.'
}
if ($env:GITHUB_REF_NAME -notin @('dev', $env:PILOT_DEFAULT_BRANCH) -or
    $env:GITHUB_REF -cne ('refs/heads/' + $env:GITHUB_REF_NAME) -or
    $env:GITHUB_REF_NAME -notmatch '^[A-Za-z0-9._/-]{1,100}$' -or
    $CandidateSha -cne $env:GITHUB_SHA) {
    throw 'Only the exact pushed or manually selected dev/default branch revision may be measured.'
}
if ($env:GITHUB_RUN_ID -notmatch '^[0-9]+$' -or $env:GITHUB_RUN_ATTEMPT -notmatch '^[0-9]+$') {
    throw 'Invalid run identity.'
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not [Security.Principal.WindowsPrincipal]::new($identity).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'VM provisioning needs the hosted administrator; benchmarks do not.'
}
$baseline = '9a340454a8292df025de368fc4b310bb736f382f'
$drive = [IO.Path]::GetPathRoot($env:SystemRoot)
$storage = [IO.DriveInfo]::new($drive)
if (-not $storage.IsReady -or $storage.DriveType -ne [IO.DriveType]::Fixed -or
    $storage.DriveFormat -cne 'NTFS') {
    throw 'Pilot storage requires the fixed NTFS system volume.'
}
$root = Join-Path $drive ("fs2-pilot-" + $env:GITHUB_RUN_ID + "-" + $env:GITHUB_RUN_ATTEMPT)
if (Test-Path -LiteralPath $root) { throw 'Refusing a reused pilot directory.' }
if ([IO.DriveInfo]::new($drive).AvailableFreeSpace -lt 10737418240) {
    throw 'Pilot needs 10 GiB free before setup; measurement retains the 8 GiB policy.'
}

$common = @'
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:Sequence = 0
$script:Placement = $null
function Assert-PilotRoot([string]$Path, [string]$OwnerSid, [string]$WorkerSid) {
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or $item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw 'Pilot root is not a regular directory.'
    }
    $acl = Get-Acl -LiteralPath $Path
    if (-not $acl.AreAccessRulesProtected -or
        $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -cne $OwnerSid) {
        throw 'Private root ownership or DACL did not bind.'
    }
    $expected = @($WorkerSid, 'S-1-5-18', 'S-1-5-32-544')
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    if ($rules.Count -ne $expected.Count) { throw 'Unexpected pilot root rule count.' }
    foreach ($sid in $expected) {
        $matches = @($rules | Where-Object { $_.IdentityReference.Value -ceq $sid })
        if ($matches.Count -ne 1 -or $matches[0].IsInherited -or
            $matches[0].AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow -or
            $matches[0].FileSystemRights -ne [Security.AccessControl.FileSystemRights]::FullControl -or
            $matches[0].InheritanceFlags -ne
                ([Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
                 [Security.AccessControl.InheritanceFlags]::ObjectInherit) -or
            $matches[0].PropagationFlags -ne [Security.AccessControl.PropagationFlags]::None) {
            throw 'Unexpected principal or access rule on the pilot root.'
        }
    }
    return $acl
}
function New-PilotRoot([string]$Path, [Security.Principal.SecurityIdentifier]$WorkerSid) {
    if (Test-Path -LiteralPath $Path) { throw 'Refusing a reused pilot directory.' }
    $token = [Security.Principal.WindowsIdentity]::GetCurrent()
    try { $owner = $token.User } finally { $token.Dispose() }
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    # Creation can assign the caller's SID without enabling restore privileges.
    $acl.SetOwner($owner)
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($sid in @($WorkerSid.Value, 'S-1-5-18', 'S-1-5-32-544')) {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
            [Security.Principal.SecurityIdentifier]::new($sid), 'FullControl',
            'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    }
    [IO.FileSystemAclExtensions]::Create([IO.DirectoryInfo]::new($Path), $acl)
    $null = Assert-PilotRoot $Path $owner.Value $WorkerSid.Value
}
function Set-PilotWorkerOwnership([string]$Path, [string]$ProvisionerSid, [string]$WorkerSid) {
    $token = [Security.Principal.WindowsIdentity]::GetCurrent()
    try {
        if ($token.User.Value -cne $WorkerSid -or
            [Security.Principal.WindowsPrincipal]::new($token).IsInRole(
                [Security.Principal.WindowsBuiltInRole]::Administrator)) {
            throw 'Only the planned standard user may take pilot ownership.'
        }
        $before = Assert-PilotRoot $Path $ProvisionerSid $WorkerSid
        $beforeDacl = $before.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access)
        # FullControl already grants WRITE_OWNER. The worker assigns only its
        # own SID; leave the protected DACL untouched and verify it afterward.
        $ownership = [Security.AccessControl.DirectorySecurity]::new()
        $ownership.SetOwner($token.User)
        [IO.FileSystemAclExtensions]::SetAccessControl([IO.DirectoryInfo]::new($Path), $ownership)
        $after = Assert-PilotRoot $Path $WorkerSid $WorkerSid
        if ($after.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access) -cne $beforeDacl) {
            throw 'Pilot ownership handoff changed the DACL.'
        }
    } finally { $token.Dispose() }
}
function Write-NewJson([string]$Path, $Value) {
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($Value | ConvertTo-Json -Depth 16))
    $file = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    try { $file.Write($bytes, 0, $bytes.Length) } finally { $file.Dispose() }
}
function Resolve-PilotApplication([string]$Name) {
    $command = Get-Command $Name -CommandType Application -ErrorAction Stop | Select-Object -First 1
    $path = $command.Source
    if (-not [IO.Path]::IsPathFullyQualified($path) -or -not [IO.File]::Exists($path)) {
        throw ('Application did not resolve to an existing absolute file: ' + $Name)
    }
    return $path
}
function Invoke-Native {
    param([string]$File, [string[]]$Arguments, [string]$Label,
          [int]$Seconds = 1200, [switch]$Benchmark, [switch]$AllowFailure)
    $script:Sequence++
    $prefix = Join-Path $Audit ($script:Sequence.ToString('D3') + '-' + $Label)
    $quoted = foreach ($argument in $Arguments) {
        if ($argument -match '["\r\n]') { throw 'Unsupported native argument spelling.' }
        '"' + [regex]::Replace($argument, '(\\+)$', '$1$1') + '"'
    }
    $self = [Diagnostics.Process]::GetCurrentProcess()
    $original = $self.ProcessorAffinity
    $child = $null
    $code = $null
    $failure = $null
    $started = [DateTime]::UtcNow.ToString('o')
    Write-Host ("Starting " + $Label)
    try {
        if (-not [IO.Path]::IsPathFullyQualified($File) -or -not [IO.File]::Exists($File)) {
            throw ('Native executable is not an existing absolute file: ' + $File)
        }
        try {
            if ($Benchmark) { $self.ProcessorAffinity = [IntPtr][long]$script:Placement.worker }
            $launch = @{
                FilePath = $File; ArgumentList = ($quoted -join ' '); PassThru = $true
                WindowStyle = 'Hidden'; RedirectStandardOutput = ($prefix + '.stdout.log')
                RedirectStandardError = ($prefix + '.stderr.log')
            }
            $child = Start-Process @launch
        } finally {
            if ($Benchmark) { $self.ProcessorAffinity = [IntPtr][long]$script:Placement.controller }
        }
        $null = $child.Handle
        if ($Benchmark -and $child.ProcessorAffinity.ToInt64() -ne $script:Placement.worker) {
            throw 'Worker did not inherit the frozen guest CPU mask.'
        }
        if (-not $child.WaitForExit($Seconds * 1000)) { throw 'Owned invocation exceeded its bound.' }
        $child.Refresh()
        $code = $child.ExitCode
        if ($null -eq $code) { throw 'Native exit code unavailable.' }
    } catch {
        $failure = $_.Exception.Message
    } finally {
        try {
            if ($null -ne $child) {
                if (-not $child.HasExited) {
                    $child.Kill($true)
                    if (-not $child.WaitForExit(10000)) { throw 'Owned process tree did not reap.' }
                }
                $child.Dispose()
            }
        } finally {
            $self.ProcessorAffinity = $original
            $receipt = [ordered]@{
                label = $Label; executable = $File; working_directory = $PWD.ProviderPath
                arguments = $Arguments; exit = $code; failure = $failure
                started_utc = $started; finished_utc = [DateTime]::UtcNow.ToString('o')
                stdout = ($prefix + '.stdout.log'); stderr = ($prefix + '.stderr.log')
                benchmark = [bool]$Benchmark; placement = $script:Placement
                restored_controller_mask = $self.ProcessorAffinity.ToInt64()
            }
            Write-NewJson ($prefix + '.receipt.json') $receipt
            $self.Dispose()
        }
    }
    if ($null -ne $failure) { throw $failure }
    if ($code -ne 0 -and -not $AllowFailure) { throw ($Label + ' failed; native exit ' + $code) }
    return [pscustomobject]$receipt
}
'@
. ([scriptblock]::Create($common))

$user = $null
$controller = $null
$result = 1
$provisionFailure = $null
$Audit = Join-Path $root 'audit'
try {
    $password = 'F2!' + [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(24)) + 'z'
    Write-Host ("::add-mask::" + $password)
    $securePassword = ConvertTo-SecureString $password -AsPlainText -Force
    $password = $null
    $user = New-LocalUser -Name 'fs2pilot' -Password $securePassword -Description 'Ephemeral benchmark worker'
    $usersGroup = Get-LocalGroup -SID ([Security.Principal.SecurityIdentifier]::new('S-1-5-32-545'))
    Add-LocalGroupMember -Group $usersGroup -Member $user
    New-PilotRoot $root $user.SID
    [IO.Directory]::CreateDirectory($Audit) | Out-Null
    Write-NewJson (Join-Path $Audit 'storage.json') @{
        role = 'system-volume'; drive = $drive; filesystem = $storage.DriveFormat
        checkout_drive = [IO.Path]::GetPathRoot($env:GITHUB_WORKSPACE)
        free_bytes = $storage.AvailableFreeSpace
        volume_acl = (Get-Acl -LiteralPath $drive).Sddl
        root_acl = (Get-Acl -LiteralPath $root).Sddl
    }
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value ("pilot_root=" + $root)
    foreach ($directory in @('home', 'home/AppData/Local', 'home/AppData/Roaming', 'temp', 'cargo/bin', 'rustup', 'target', 'fixture')) {
        [IO.Directory]::CreateDirectory((Join-Path $root $directory)) | Out-Null
    }

    $git = Resolve-PilotApplication 'git.exe'
    $checkout = Invoke-Native $git @('-C', $env:GITHUB_WORKSPACE, 'rev-parse', 'HEAD') 'workflow-head'
    if ([IO.File]::ReadAllText($checkout.stdout).Trim() -cne $WorkflowSha) { throw 'Workflow SHA mismatch.' }
    $null = Invoke-Native $git @('-C', $env:GITHUB_WORKSPACE, 'merge-base', '--is-ancestor', $CandidateSha, ('refs/remotes/origin/' + $env:GITHUB_REF_NAME)) 'candidate-branch-ancestry'
    $null = Invoke-Native $git @('-C', $env:GITHUB_WORKSPACE, 'merge-base', '--is-ancestor', $baseline, $WorkflowSha) 'harness-ancestry'
    $null = Invoke-Native $git @('-C', $env:GITHUB_WORKSPACE, 'merge-base', '--is-ancestor', $baseline, $CandidateSha) 'baseline-ancestry'
    if ($CandidateSha -eq $baseline) { throw 'A/B requires distinct source commits.' }
    $repo = Join-Path $root 'repo'
    $baseCache = Join-Path $root 'baseline-cache'
    foreach ($destination in @($repo, $baseCache)) {
        $null = Invoke-Native $git @('clone', '--no-local', '--no-hardlinks', '--no-checkout', '--quiet', $env:GITHUB_WORKSPACE, $destination) ('clone-' + [IO.Path]::GetFileName($destination))
    }
    $null = Invoke-Native $git @('-C', $repo, 'checkout', '--detach', $WorkflowSha) 'checkout-runner'
    $null = Invoke-Native $git @('-C', $baseCache, 'checkout', '--detach', $baseline) 'checkout-prefetch'
    $proxy = Resolve-PilotApplication 'rustup.exe'
    foreach ($name in @('rustup.exe', 'cargo.exe', 'rustc.exe')) {
        Copy-Item -LiteralPath $proxy -Destination (Join-Path $root ('cargo/bin/' + $name))
    }

    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Fs2PilotTopology {
    [DllImport("kernel32.dll", SetLastError=true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool GetLogicalProcessorInformation(IntPtr buffer, ref uint length);
}
'@
    if ([IntPtr]::Size -ne 8 -or [int]$env:NUMBER_OF_PROCESSORS -ne 4) {
        throw 'This pilot contract requires the standard four-vCPU x64 Windows runner.'
    }
    $self = [Diagnostics.Process]::GetCurrentProcess()
    $allowed = $self.ProcessorAffinity.ToInt64()
    $self.Dispose()
    if ($allowed -ne 15) { throw 'Expected access to all four guest vCPUs.' }
    [uint32]$length = 0
    $sized = [Fs2PilotTopology]::GetLogicalProcessorInformation([IntPtr]::Zero, [ref]$length)
    if ($sized -or [Runtime.InteropServices.Marshal]::GetLastWin32Error() -ne 122 -or
        $length -eq 0 -or $length -gt 65536 -or $length % 32 -ne 0) {
        throw 'Guest topology sizing failed.'
    }
    $buffer = [Runtime.InteropServices.Marshal]::AllocHGlobal([int]$length)
    $cores = @()
    try {
        $capacity = $length
        if (-not [Fs2PilotTopology]::GetLogicalProcessorInformation($buffer, [ref]$length) -or $length -ne $capacity) {
            throw 'Guest topology changed or could not be read.'
        }
        for ($offset = 0; $offset -lt $length; $offset += 32) {
            if ([Runtime.InteropServices.Marshal]::ReadInt32($buffer, $offset + 8) -eq 0) {
                $cores += [Runtime.InteropServices.Marshal]::ReadInt64($buffer, $offset)
            }
        }
    } finally { [Runtime.InteropServices.Marshal]::FreeHGlobal($buffer) }
    [long]$coverage = 0
    foreach ($core in $cores) {
        if ($core -le 0 -or ($core -band $allowed) -ne $core -or ($coverage -band $core) -ne 0) {
            throw 'Guest core topology is incomplete or overlapping.'
        }
        $coverage = $coverage -bor $core
    }
    if ($coverage -ne $allowed -or $cores.Count -lt 2) { throw 'No disjoint guest core placement.' }
    $physical = [long]($cores | Sort-Object | Select-Object -Last 1)
    [long]$workerMask = 8
    while (($workerMask -band $physical) -eq 0) { $workerMask = $workerMask -shr 1 }
    $placement = @{ worker = $workerMask; physical = $physical; controller = ($allowed -band (-bnot $physical)); allowed = $allowed }
    $manifest = @{
        schema_version = 1; baseline = $baseline; candidate = $CandidateSha; workflow_sha = $WorkflowSha
        branch = $env:GITHUB_REF_NAME; default_branch = $env:PILOT_DEFAULT_BRANCH
        run_id = $env:GITHUB_RUN_ID; run_attempt = $env:GITHUB_RUN_ATTEMPT
        dataset = 'github-hosted-windows-2022-pilot'; performance_evidence_scope = 'pilot workloads on this VM only'
        physical_host_isolation = 'unknown'; guest_core_masks = $cores; placement = $placement
        root = $root; repo = $repo; baseline_cache = $baseCache; user_sid = $user.SID.Value
        provisioner_sid = $identity.User.Value
        image_os = $env:ImageOS; image_version = $env:ImageVersion; runner_arch = $env:RUNNER_ARCH
        os = (Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber)
        processors = @(Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors)
        volume = @(Get-Volume -DriveLetter $drive.Substring(0, 1) | Select-Object DriveLetter, FileSystem, Size, SizeRemaining, HealthStatus)
        rust = '1.98.1'; msrv = '1.88.0'; path = $env:PATH; git = $git
        proxy_sha256 = (Get-FileHash -LiteralPath $proxy -Algorithm SHA256).Hash.ToLowerInvariant()
        profiles = @('duplicate-single-refs', 'file-create-delete-refs', 'lock-refs')
        mean_limit_percent = 5; peak_limit_percent = 20; retries = 0; tracing = $false
        desktop_policy = 'noninteractive CI worker; no desktop-lock assertion'
        control = 'upstream A/A in every profile'; profile_timeout_seconds = 4200
        controller_timeout_seconds = 16200; created_utc = [DateTime]::UtcNow.ToString('o')
    }
    Write-NewJson (Join-Path $Audit 'plan.json') $manifest

    $worker = @'
$plan = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'audit/plan.json') -Raw | ConvertFrom-Json
$Audit = Join-Path $plan.root 'audit/worker'
[IO.Directory]::CreateDirectory($Audit) | Out-Null
$receipts = @()
$failure = $null
$allPassed = $false
try {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if ($identity.User.Value -cne $plan.user_sid -or $principal.IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Worker is not the planned standard user.' }
    Set-PilotWorkerOwnership $plan.root $plan.provisioner_sid $plan.user_sid
    # Retain only machine/tool setup variables, never the Actions runtime credentials.
    $keep = @('SystemRoot', 'WINDIR', 'ComSpec', 'PATHEXT', 'ProgramFiles', 'ProgramFiles(x86)',
              'ProgramW6432', 'ProgramData', 'ALLUSERSPROFILE', 'COMPUTERNAME',
              'NUMBER_OF_PROCESSORS', 'PROCESSOR_IDENTIFIER', 'PROCESSOR_ARCHITECTURE',
              'PROCESSOR_LEVEL', 'PROCESSOR_REVISION', 'OS')
    foreach ($item in @(Get-ChildItem Env:)) {
        if ($item.Name -notin $keep) { [Environment]::SetEnvironmentVariable($item.Name, $null, 'Process') }
    }
    $env:USERPROFILE = Join-Path $plan.root 'home'
    $env:LOCALAPPDATA = Join-Path $env:USERPROFILE 'AppData/Local'
    $env:APPDATA = Join-Path $env:USERPROFILE 'AppData/Roaming'
    $env:TEMP = Join-Path $plan.root 'temp'
    $env:TMP = $env:TEMP
    $env:CARGO_HOME = Join-Path $plan.root 'cargo'
    $env:RUSTUP_HOME = Join-Path $plan.root 'rustup'
    $env:CARGO_TARGET_DIR = Join-Path $plan.root 'target'
    $env:CARGO_BUILD_JOBS = '1'
    $env:CARGO_INCREMENTAL = '0'
    $env:RUSTUP_TOOLCHAIN = '1.98.1'
    $env:CARGO = Join-Path $env:CARGO_HOME 'bin/cargo.exe'
    $env:PATH = (Join-Path $env:CARGO_HOME 'bin') + ';' + $plan.path
    $env:FS2_PAIRED_DIAGNOSTIC_SAMPLES = '0'
    $env:GIT_CONFIG_NOSYSTEM = '1'
    $env:GIT_CONFIG_GLOBAL = Join-Path $env:USERPROFILE 'gitconfig'
    $safeRepo = $plan.repo.Replace('\', '/')
    $safeCache = $plan.baseline_cache.Replace('\', '/')
    [IO.File]::WriteAllLines($env:GIT_CONFIG_GLOBAL, @('[safe]', ('directory = ' + $safeRepo),
        ('directory = ' + $safeCache), '[core]', 'longpaths = true'), [Text.UTF8Encoding]::new($false))
    Set-Location -LiteralPath $plan.repo
    Write-NewJson (Join-Path $Audit 'token.json') @{
        user_sid = $identity.User.Value; elevated = $false; credential_environment = 'allowlisted'
        root_owner_sid = $identity.User.Value; root_dacl_handoff = 'verified unchanged'
        desktop = 'noninteractive'; guest_topology_only = $true
    }
    $null = Invoke-Native (Get-Process -Id $PID).Path @('-NoProfile', '-NonInteractive', '-File',
        (Join-Path $plan.repo 'tools/ci/tests/windows-benchmark-pilot-ownership.ps1'),
        '-EvidenceRoot', (Join-Path $plan.root 'ownership-checks')) 'ownership-regression'
    $null = Invoke-Native (Get-Process -Id $PID).Path @('-NoProfile', '-NonInteractive', '-File',
        (Join-Path $plan.repo 'tools/ci/tests/windows-benchmark-pilot-launch.ps1'),
        '-EvidenceRoot', (Join-Path $plan.root 'launch-checks')) 'launch-regression'
    $rustup = Join-Path $env:CARGO_HOME 'bin/rustup.exe'
    foreach ($version in @('1.88.0', '1.98.1')) {
        $null = Invoke-Native $rustup @('toolchain', 'install', $version, '--profile', 'minimal') ('install-' + $version)
    }
    $null = Invoke-Native $env:CARGO @('+1.98.1', 'fetch', '--locked') 'prefetch-runner'
    $null = Invoke-Native $env:CARGO @('+1.98.1', 'fetch', '--manifest-path', (Join-Path $plan.baseline_cache 'Cargo.toml'),
        '--target', 'x86_64-pc-windows-msvc') 'prefetch-upstream'
    $env:CARGO_NET_OFFLINE = 'true'
    $null = Invoke-Native $env:CARGO @('+1.88.0', 'test', '--package', 'fs2-turbo', '--locked', '--offline') 'msrv-tests'
    $null = Invoke-Native $env:CARGO @('+1.98.1', 'test', '--package', 'fs2-dev', '--locked', '--offline') 'tooling-tests'
    $null = Invoke-Native $env:CARGO @('+1.98.1', 'build', '--release', '--package', 'fs2-dev', '--locked', '--offline') 'build-runner'
    $runner = Join-Path $env:CARGO_TARGET_DIR 'release/fs2-dev.exe'
    $sourceHashes = @{}
    foreach ($path in @('benchmarks/paired.rs', 'benchmarks/paired_protocol.rs', 'benchmarks/paired_common.rs',
        'benchmarks/paired_lock_refs.rs', 'benchmarks/paired_lock_protocol.rs',
        'benchmarks/paired_single_duplicate_protocol.rs', 'benchmarks/paired_file_create_delete_protocol.rs',
        'benchmarks/measurement-policy.json', 'benchmarks/duplicate-measurement-policy.json',
        'tools/fs2-dev/src/benchmark/host.rs', 'tools/fs2-dev/src/benchmark/host/windows.rs')) {
        $sourceHashes[$path] = (Get-FileHash -LiteralPath (Join-Path $plan.repo $path) -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    Write-NewJson (Join-Path $Audit 'runner.json') @{
        workflow_sha = $plan.workflow_sha; baseline = $plan.baseline; candidate = $plan.candidate
        runner_sha256 = (Get-FileHash -LiteralPath $runner -Algorithm SHA256).Hash.ToLowerInvariant()
        source_hashes = $sourceHashes
    }
    $script:Placement = $plan.placement
    $self = [Diagnostics.Process]::GetCurrentProcess()
    if ($self.ProcessorAffinity.ToInt64() -ne $plan.placement.allowed) { throw 'Worker inherited an unexpected affinity.' }
    $self.Dispose()
    foreach ($profile in $plan.profiles) {
        $outputRoot = Join-Path $plan.root 'measurements'
        $output = Join-Path $outputRoot $profile
        $arguments = @('bench', $profile, '--baseline', $plan.baseline, '--candidate', $plan.candidate,
            '--trust-selected-code', '--repo', $plan.repo, '--fixture', (Join-Path $plan.root 'fixture'),
            '--output-root', $outputRoot, '--output', $output,
            '--idle-max-core-busy-percent', '5', '--idle-max-sample-busy-percent', '20')
        $receipt = Invoke-Native -File $runner -Arguments $arguments -Label $profile -Seconds 4200 -Benchmark -AllowFailure
        $receipts += $receipt
        $reportPath = Join-Path $output 'report.json'
        if (-not (Test-Path -LiteralPath $reportPath)) { throw 'Canonical report missing; remaining profiles deferred.' }
        $report = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
        if (-not $report.PSObject.Properties['processes'] -or
            -not $report.processes.PSObject.Properties['runs'] -or @($report.processes.runs).Count -eq 0) {
            throw 'Admission or setup refused; no retry or replacement window.'
        }
        foreach ($run in $report.processes.runs) {
            if ($run.process.outcome.kind -ne 'exited' -or $run.process.outcome.code -ne 0) {
                throw 'Measurement execution failed; remaining profiles deferred.'
            }
        }
        if ($profile -ne $plan.profiles[-1]) { Start-Sleep -Seconds 120 }
    }
    $allPassed = $receipts.Count -eq $plan.profiles.Count -and @($receipts | Where-Object { $_.exit -ne 0 }).Count -eq 0
} catch {
    $failure = $_.Exception.Message
} finally {
    Write-NewJson (Join-Path $Audit 'pilot-result.json') @{
        dataset = $plan.dataset; all_profile_gates_passed = $allPassed
        performance_claim = 'not a full-suite or physical-host performance result'
        attempted = $receipts.Count; planned = $plan.profiles.Count; invocations = $receipts
        failure = $failure; finished_utc = [DateTime]::UtcNow.ToString('o')
    }
}
if ($null -ne $failure) { Write-Error $failure -ErrorAction Continue }
if (-not $allPassed) { exit 1 }
Write-Host 'PILOT FINISHED: all three profiles passed; evidence applies only to this hosted VM.'
exit 0
'@
    $workerPath = Join-Path $root 'worker.ps1'
    [IO.File]::WriteAllText($workerPath, $common + [Environment]::NewLine + $worker, [Text.UTF8Encoding]::new($false))
    $credential = [pscredential]::new(($env:COMPUTERNAME + '\' + $user.Name), $securePassword)
    $launch = @{
        FilePath = (Get-Process -Id $PID).Path
        ArgumentList = @('-NoProfile', '-NonInteractive', '-File', $workerPath)
        Credential = $credential; LoadUserProfile = $true; PassThru = $true; WindowStyle = 'Hidden'
        WorkingDirectory = $root; RedirectStandardOutput = (Join-Path $Audit 'controller.stdout.log')
        RedirectStandardError = (Join-Path $Audit 'controller.stderr.log')
    }
    $controller = Start-Process @launch
    $null = $controller.Handle
    if (-not $controller.WaitForExit(16200000)) { throw 'Fixed pilot controller deadline exceeded.' }
    $controller.Refresh()
    if ($null -eq $controller.ExitCode) { throw 'Controller native exit unavailable.' }
    $result = $controller.ExitCode
} catch {
    $provisionFailure = $_.Exception.Message
    Write-Error $provisionFailure -ErrorAction Continue
} finally {
    try {
        if ($null -ne $controller) {
            if (-not $controller.HasExited) {
                $controller.Kill($true)
                if (-not $controller.WaitForExit(10000)) { throw 'Owned worker tree did not reap.' }
            }
            $controller.Dispose()
        }
    } finally {
        if ($null -ne $user) { Remove-LocalUser -SID $user.SID }
        if (Test-Path -LiteralPath $Audit) {
            Write-NewJson (Join-Path $Audit 'launcher-result.json') @{
                exit = $result; failure = $provisionFailure
                finished_utc = [DateTime]::UtcNow.ToString('o'); retained_root = $root
            }
        }
    }
}
Write-Host ("PILOT FINISHED: native exit " + $result + "; retained evidence: " + $Audit)
exit $result
