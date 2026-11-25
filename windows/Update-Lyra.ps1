param(
    [Parameter(Mandatory)][string]$Root,
    [Parameter(Mandatory)][string]$Log,
    [switch]$RequireCheck
)

$ErrorActionPreference = 'Stop'
$repository = 'nevrozza/lyra-ai'
$versions = Join-Path $Root 'target\lyra-updates'
$current = Join-Path $versions 'current.txt'

function Write-Update($Message) {
    Write-Host $Message
    Add-Content -LiteralPath $Log -Encoding UTF8 -Value $Message
}

function Get-ArchiveVersion($Revision) {
    if ($Revision -cnotmatch '^[0-9a-f]{40}$') { throw 'Invalid update revision.' }
    Join-Path $versions $Revision
}

function Test-ArchiveVersion($Directory) {
    (Test-Path -LiteralPath (Join-Path $Directory 'Start-Lyra.ps1') -PathType Leaf) -and
    (Test-Path -LiteralPath (Join-Path $Directory 'Cargo.lock') -PathType Leaf) -and
    (Test-Path -LiteralPath (Join-Path $Directory 'windows\Update-Lyra.ps1') -PathType Leaf)
}

function Receive-ArchiveVersion($Revision) {
    $version = Get-ArchiveVersion $Revision
    if (Test-ArchiveVersion $version) { return $version }
    $staging = Join-Path $versions ([Guid]::NewGuid().ToString('N'))
    [void][IO.Directory]::CreateDirectory($staging)
    try {
        $zip = Join-Path $staging 'source.zip'
        Invoke-WebRequest -UseBasicParsing -Uri "https://codeload.github.com/$repository/zip/$Revision" -TimeoutSec 30 -OutFile $zip
        $expanded = Join-Path $staging 'source'
        Expand-Archive -LiteralPath $zip -DestinationPath $expanded
        $roots = @(Get-ChildItem -LiteralPath $expanded -Directory)
        if ($roots.Count -ne 1 -or -not (Test-ArchiveVersion $roots[0].FullName)) {
            throw 'The downloaded archive is missing the Lyra launcher files.'
        }
        [IO.Directory]::Move($roots[0].FullName, $version)
        return $version
    } finally {
        Remove-Item -LiteralPath $staging -Recurse -Force
    }
}

$git = Get-Command git.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
if ($git -and (Test-Path -LiteralPath (Join-Path $Root '.git'))) {
    $changes = & $git.Source -C $Root status --porcelain --untracked-files=no
    if ($LASTEXITCODE -ne 0) {
        if ($RequireCheck) { throw 'Could not check the Git working tree.' }
        Write-Update 'Could not check the Git working tree. Starting the installed version.'
        return $Root
    }
    if ($changes) {
        if ($RequireCheck) { throw 'Local Git changes detected. Finish those changes before updating Lyra.' }
        Write-Update 'Local Git changes detected. Skipping updates and starting the working copy.'
        return $Root
    }
    Write-Update 'Checking for updates with git pull...'
    $previousPrompt = $env:GIT_TERMINAL_PROMPT
    $previousInteractive = $env:GCM_INTERACTIVE
    $env:GIT_TERMINAL_PROMPT = '0'
    $env:GCM_INTERACTIVE = 'Never'
    try {
        # Windows PowerShell turns native stderr into error records when merging streams.
        $ErrorActionPreference = 'Continue'
        & $git.Source -C $Root -c http.lowSpeedLimit=1 -c http.lowSpeedTime=10 pull --ff-only --no-rebase 2>&1 |
            ForEach-Object { Write-Update $_.ToString() }
        if ($LASTEXITCODE -eq 0) { return $Root }
    } finally {
        $ErrorActionPreference = 'Stop'
        $env:GIT_TERMINAL_PROMPT = $previousPrompt
        $env:GCM_INTERACTIVE = $previousInteractive
    }
    Write-Update 'Git pull failed. Checking for an archive update.'
} else {
    Write-Update 'Checking for an archive update.'
}

$installed = $Root
try {
    if (Test-Path -LiteralPath $current -PathType Leaf) {
        $cachedRevision = ([string](Get-Content -LiteralPath $current -Raw)).Trim()
        if ($cachedRevision -cmatch '^[0-9a-f]{40}$') {
            $cached = Get-ArchiveVersion $cachedRevision
            if (Test-ArchiveVersion $cached) { $installed = $cached }
        }
    }

    $revision = (Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/commits/main" -TimeoutSec 10).sha
    $version = Receive-ArchiveVersion $revision
    Set-Content -LiteralPath $current -Value $revision -Encoding ASCII
    $installed = $version
    Write-Update "GitHub archive version: $revision"
} catch {
    if ($RequireCheck) {
        Write-Update ('Archive update unavailable. ' + $_.Exception.Message)
        throw
    }
    Write-Update ('Archive update unavailable. Starting the installed version. ' + $_.Exception.Message)
}
return $installed
