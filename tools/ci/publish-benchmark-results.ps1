#requires -Version 7.0
param([Parameter(Mandatory)][ValidateSet('Check', 'Restore', 'Render')][string]$Phase)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -cne 'true' -or $env:GITHUB_EVENT_NAME -cne 'workflow_run' -or
    $env:GITHUB_REPOSITORY -cne 'arthurianresolve/fs2-turbo') {
    throw 'Accepted-results preparation is restricted to the trusted workflow.'
}
switch ($Phase) {
    'Check' {
        if ($env:SUBJECT_BRANCH -notmatch '^[A-Za-z0-9._/-]{1,100}$' -or
            $env:SUBJECT_SHA -notmatch '^[0-9a-f]{40}$') { throw 'Invalid source identity.' }
        $branch = [Uri]::EscapeDataString($env:SUBJECT_BRANCH)
        $head = & gh api "repos/arthurianresolve/fs2-turbo/git/ref/heads/$branch" --jq '.object.sha'
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        $current = $head.Trim() -ceq $env:SUBJECT_SHA
        Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value ("current=" + $current.ToString().ToLowerInvariant())
    }
    'Restore' {
        $state = Join-Path $env:RUNNER_TEMP 'benchmark-previous.json'
        $url = 'https://arthurianresolve.github.io/fs2-turbo/accepted.json?run=' + $env:GITHUB_RUN_ID
        $status = & curl --silent --show-error --max-time 60 --max-filesize 67108864 --proto '=https' --output $state --write-out '%{http_code}' $url
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        if ($status -ceq '404') {
            $history = & gh api 'repos/arthurianresolve/fs2-turbo/deployments?environment=github-pages&per_page=1' --jq 'length'
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
            if ($history.Trim() -cne '0') { throw 'Existing Pages state is unavailable; refusing to erase accepted results.' }
            [IO.File]::WriteAllText($state, '{"schema_version":1,"branches":{}}', [Text.UTF8Encoding]::new($false))
        } elseif ($status -cne '200') {
            throw "Accepted-state request failed: HTTP $status"
        }
    }
    'Render' {
        & rustup toolchain install 1.98.1 --profile minimal
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        & cargo +1.98.1 run --package fs2-dev --locked -- bench pages --input (Join-Path $env:RUNNER_TEMP 'benchmark-evidence') --previous (Join-Path $env:RUNNER_TEMP 'benchmark-previous.json') --output (Join-Path $env:RUNNER_TEMP 'benchmark-site') --branch $env:SUBJECT_BRANCH --candidate $env:SUBJECT_SHA --run-id $env:SUBJECT_RUN_ID --attempt $env:SUBJECT_ATTEMPT
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value 'ready=true'
    }
}
exit 0
