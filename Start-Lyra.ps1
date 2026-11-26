param(
    [string]$Model = '',
    [ValidateRange(1, 65535)]
    [int]$Port = 8787,
    [switch]$CloseStartupConsole,
    [string]$WorkerDirectory,
    [string]$BuildDirectory,
    [switch]$UpdateAndRestart,
    [int]$PreviousWorkerId,
    [string]$RunningRevision,
    [string]$UpdateResult
)

$ErrorActionPreference = 'Stop'

function Update-LyraShortcut($Directory) {
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut((Join-Path $Directory 'Lyra AI.lnk'))
    $shortcut.TargetPath = Join-Path $Directory 'Start-Lyra.cmd'
    $shortcut.Arguments = ''
    $shortcut.WorkingDirectory = $Directory
    $shortcut.IconLocation = (Join-Path $Directory 'assets\lyra.ico') + ',0'
    $shortcut.Description = 'Start Lyra AI'
    $shortcut.Save()
}

function Start-LyraWorker($SourceRoot, $Directory) {
    $arguments = '-NoProfile -NonInteractive -STA -ExecutionPolicy Bypass -File "{0}" -Port {1} -WorkerDirectory "{2}" -BuildDirectory "{3}"' -f
        (Join-Path $SourceRoot 'Start-Lyra.ps1'), $Port, $Directory, $BuildDirectory
    if ($Model) {
        if ($Model -cnotmatch '^[a-zA-Z0-9][a-zA-Z0-9._:/-]*\z') { throw 'Invalid model name.' }
        $arguments += ' -Model "' + $Model + '"'
    }
    $output = Join-Path $Directory 'startup.log'
    $ready = Join-Path $Directory 'ready'
    $printed = (Get-Item -LiteralPath $output).Length
    $startInfo = [Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'powershell.exe'), $arguments)
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $worker = [Diagnostics.Process]::Start($startInfo)
    try {
        $stream = [IO.File]::Open($output, 'Open', 'Read', 'ReadWrite')
        $stream.Position = $printed
        $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8, $false)
        try {
            $buffer = New-Object char[] 16384
            while ($true) {
                $finished = (Test-Path -LiteralPath $ready) -or $worker.HasExited
                $count = $reader.Read($buffer, 0, $buffer.Length)
                if ($count -gt 0) { [Console]::Write($buffer, 0, $count) }
                if ($finished -and $count -eq 0) { break }
                Start-Sleep -Milliseconds 150
            }
        } finally { $reader.Dispose() }
        if ($worker.HasExited) {
            throw "Lyra exited during startup (code $($worker.ExitCode))."
        }
    } finally { $worker.Dispose() }
}

function Get-LyraRevision($Directory) {
    $archive = Split-Path -Leaf $Directory
    if ($archive -cmatch '^[0-9a-f]{40}$' -and (Split-Path -Parent $Directory) -eq (Join-Path $BuildDirectory 'lyra-updates')) {
        return $archive
    }
    if (-not (Test-Path -LiteralPath (Join-Path $Directory '.git'))) { return }
    $git = Get-Command git.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($git) {
        $revision = & $git.Source -C $Directory rev-parse --verify HEAD
        if ($LASTEXITCODE -eq 0 -and $revision -cmatch '^[0-9a-f]{40}$') { return $revision }
    }
}

function Write-UpdateResult($Status, $Message = '') {
    $pending = $UpdateResult + '.tmp'
    [IO.File]::WriteAllText($pending, "$Status`n$Message")
    [IO.File]::Move($pending, $UpdateResult)
}

function Find-Application($Names, $Fallbacks, [switch]$Optional) {
    $command = Get-Command -Name $Names -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) { return $command.Source }
    foreach ($path in $Fallbacks) {
        if (Test-Path -LiteralPath $path -PathType Leaf) { return $path }
    }
    if (-not $Optional) {
        throw "Cannot find $($Names -join ' or '). Install it and reopen Windows Terminal or sign in again to refresh PATH."
    }
}

try {
    if (-not $WorkerDirectory) {
        if ($UpdateAndRestart) {
            if (-not $BuildDirectory -or -not $UpdateResult -or $PreviousWorkerId -le 0) {
                throw 'Missing tray restart parameters.'
            }
            $previousWorker = [Diagnostics.Process]::GetProcessById($PreviousWorkerId)
        } else {
            $BuildDirectory = Join-Path $PSScriptRoot 'target'
        }
        $installation = [IO.Path]::GetDirectoryName($BuildDirectory)
        Write-Host 'Lyra AI startup'
        $startup = [Threading.Mutex]::new($false, 'Local\LyraAI.Startup')
        $ownsStartup = $false
        try {
            $startupWait = if ($UpdateAndRestart) { 5000 } else { 0 }
            try { $ownsStartup = $startup.WaitOne($startupWait) }
            catch [Threading.AbandonedMutexException] { $ownsStartup = $true }
            if (-not $ownsStartup) { throw 'Lyra is already starting.' }
            $tray = $null
            if (-not $UpdateAndRestart -and [Threading.Mutex]::TryOpenExisting('Local\LyraAI.Tray', [ref]$tray)) {
                $tray.Dispose()
                throw 'Lyra is already running. Use its tray icon.'
            }
            $sessions = Join-Path $env:LOCALAPPDATA 'LyraAI\startup'
            Get-ChildItem -LiteralPath $sessions -Directory -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -cmatch '^[0-9a-f]{32}$' -and $_.LastWriteTime -lt (Get-Date).AddDays(-7) } |
                Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
            $session = Join-Path $sessions ([Guid]::NewGuid().ToString('N'))
            [void][IO.Directory]::CreateDirectory($session)
            $startupLog = Join-Path $session 'startup.log'
            Set-Content -LiteralPath $startupLog -Encoding UTF8 -Value 'Lyra AI startup'
            $source = $installation
            if ($UpdateAndRestart) {
                $source = & "$PSScriptRoot\windows\Update-Lyra.ps1" -Root $installation -Log $startupLog -RequireCheck
                $revision = Get-LyraRevision $source
                if (-not $revision) { throw 'Could not identify the available Lyra version.' }
                if ($revision -eq $RunningRevision) {
                    Write-UpdateResult 'current'
                    return
                }
                Write-UpdateResult 'restart'
                $restartRequested = $true
                # The tray shuts down its child processes before this worker exits.
                if (-not $previousWorker.WaitForExit(30000)) { throw 'The previous Lyra process did not exit.' }
            } else {
                try { $source = & "$PSScriptRoot\windows\Update-Lyra.ps1" -Root $installation -Log $startupLog }
                catch {
                    $message = "Update check failed. Starting the installed version. $($_.Exception.Message)"
                    Write-Warning $message
                    Add-Content -LiteralPath $startupLog -Encoding UTF8 -Value $message
                }
            }
            Start-LyraWorker $source $session
        } finally {
            if ($ownsStartup) { $startup.ReleaseMutex() }
            $startup.Dispose()
        }
        return
    }
    $logStream = [IO.File]::Open((Join-Path $WorkerDirectory 'startup.log'), 'Append', 'Write', 'ReadWrite')
    $logWriter = [IO.StreamWriter]::new($logStream, [Text.Encoding]::UTF8)
    $logWriter.AutoFlush = $true
    [Console]::SetOut($logWriter)
    # The worker can run from an update archive. The build directory belongs to the installation.
    try { Update-LyraShortcut ([IO.Path]::GetDirectoryName($BuildDirectory)) }
    catch { [Console]::WriteLine("Could not update the Lyra AI shortcut: $($_.Exception.Message)") }
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    [Console]::WriteLine('Checking Cargo, Ollama and Pi...')
    $cargo = Find-Application @('cargo.exe') @("$env:USERPROFILE\.cargo\bin\cargo.exe")
    $ollama = Find-Application @('ollama.exe') @("$env:LOCALAPPDATA\Programs\Ollama\ollama.exe") -Optional
    $pi = Find-Application @('pi.exe', 'pi.cmd') @("$env:USERPROFILE\.pi\agent\bin\pi.cmd", "$env:LOCALAPPDATA\pi-node\current\pi.cmd", "$env:APPDATA\npm\pi.cmd")
    $env:PATH = "$(Split-Path -Parent $pi);$(Split-Path -Parent $cargo);$env:PATH"
    if (-not ('LyraTray.Session' -as [type])) {
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        $references = @('System.dll', 'System.Core.dll', 'System.Windows.Forms.dll', 'System.Drawing.dll',
            'System.Net.Http.dll', 'System.Web.Extensions.dll',
            [Windows.Automation.AutomationElement].Assembly.Location,
            [Windows.Automation.ControlType].Assembly.Location)
        Add-Type -Path @("$PSScriptRoot\windows\ProcessJob.cs", "$PSScriptRoot\windows\Tray.cs",
            "$PSScriptRoot\windows\BrowserTab.cs", "$PSScriptRoot\windows\InstalledDashboard.cs") -ReferencedAssemblies $references
    }
    $revision = ''
    try { $revision = [string](Get-LyraRevision $PSScriptRoot) }
    catch { [Console]::WriteLine("Could not identify this Lyra version: $($_.Exception.Message)") }
    [LyraTray.Session]::Run($PSScriptRoot, $cargo, $ollama, $Model, $Port, $WorkerDirectory, $BuildDirectory, $revision)
} catch {
    $message = $_.Exception.GetBaseException().Message
    if ($UpdateAndRestart -and $UpdateResult) {
        if ($restartRequested) {
            Add-Type -AssemblyName System.Windows.Forms
            [void][Windows.Forms.MessageBox]::Show("$message`n`nStartup log: $startupLog", 'Lyra AI update')
        } else {
            Write-UpdateResult 'error' $message
        }
    }
    [Console]::WriteLine($message)
    if ($CloseStartupConsole) { [void](Read-Host 'Press Enter to close') }
    exit 1
} finally {
    if ($previousWorker) { $previousWorker.Dispose() }
    if ($logWriter) { $logWriter.Dispose() }
}
