param(
    [string]$Model = '',
    [ValidateRange(1, 65535)]
    [int]$Port = 8787,
    [switch]$CloseStartupConsole,
    [string]$WorkerDirectory,
    [string]$BuildDirectory
)

$ErrorActionPreference = 'Stop'

function Update-LyraShortcut {
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut((Join-Path $PSScriptRoot 'Lyra AI.lnk'))
    $shortcut.TargetPath = Join-Path $PSHOME 'powershell.exe'
    $startupScript = Join-Path $PSScriptRoot 'Start-Lyra.ps1'
    $shortcut.Arguments = '-NoProfile -STA -ExecutionPolicy Bypass -File "' +
        $startupScript + '" -CloseStartupConsole'
    $shortcut.WorkingDirectory = $PSScriptRoot
    $shortcut.IconLocation = (Join-Path $PSScriptRoot 'assets\lyra.ico') + ',0'
    $shortcut.Description = 'Start Lyra AI'
    $shortcut.Save()
}

function Start-LyraWorker($SourceRoot, $Directory) {
    $arguments = '-NoProfile -NonInteractive -STA -ExecutionPolicy Bypass -File "{0}" -Port {1} -WorkerDirectory "{2}" -BuildDirectory "{3}"' -f
        (Join-Path $SourceRoot 'Start-Lyra.ps1'), $Port, $Directory, (Join-Path $PSScriptRoot 'target')
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

function Find-Application($Names, $Fallbacks) {
    $command = Get-Command -Name $Names -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) { return $command.Source }
    foreach ($path in $Fallbacks) {
        if (Test-Path -LiteralPath $path -PathType Leaf) { return $path }
    }
    throw "Cannot find $($Names -join ' or '). Install it and reopen Windows Terminal or sign in again to refresh PATH."
}

try {
    if (-not $WorkerDirectory) {
        Write-Host 'Lyra AI startup'
        $startup = [Threading.Mutex]::new($false, 'Local\LyraAI.Startup')
        $ownsStartup = $false
        try {
            try { $ownsStartup = $startup.WaitOne(0) }
            catch [Threading.AbandonedMutexException] { $ownsStartup = $true }
            if (-not $ownsStartup) { throw 'Lyra is already starting.' }
            $tray = $null
            if ([Threading.Mutex]::TryOpenExisting('Local\LyraAI.Tray', [ref]$tray)) {
                $tray.Dispose()
                throw 'Lyra is already running. Use its tray icon.'
            }
            try { Update-LyraShortcut }
            catch { Write-Warning "Could not update the Lyra AI shortcut: $($_.Exception.Message)" }
            $sessions = Join-Path $env:LOCALAPPDATA 'LyraAI\startup'
            Get-ChildItem -LiteralPath $sessions -Directory -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -cmatch '^[0-9a-f]{32}$' -and $_.LastWriteTime -lt (Get-Date).AddDays(-7) } |
                Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
            $session = Join-Path $sessions ([Guid]::NewGuid().ToString('N'))
            [void][IO.Directory]::CreateDirectory($session)
            $startupLog = Join-Path $session 'startup.log'
            Set-Content -LiteralPath $startupLog -Encoding UTF8 -Value 'Lyra AI startup'
            $source = $PSScriptRoot
            try { $source = & "$PSScriptRoot\windows\Update-Lyra.ps1" -Root $PSScriptRoot -Log $startupLog }
            catch {
                $message = "Update check failed. Starting the installed version. $($_.Exception.Message)"
                Write-Warning $message
                Add-Content -LiteralPath $startupLog -Encoding UTF8 -Value $message
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
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    [Console]::WriteLine('Checking Cargo, Ollama and Pi...')
    $cargo = Find-Application @('cargo.exe') @("$env:USERPROFILE\.cargo\bin\cargo.exe")
    $ollama = Find-Application @('ollama.exe') @("$env:LOCALAPPDATA\Programs\Ollama\ollama.exe")
    $pi = Find-Application @('pi.exe', 'pi.cmd') @("$env:USERPROFILE\.pi\agent\bin\pi.cmd", "$env:LOCALAPPDATA\pi-node\current\pi.cmd", "$env:APPDATA\npm\pi.cmd")
    $env:PATH = "$(Split-Path -Parent $pi);$(Split-Path -Parent $cargo);$env:PATH"
    if (-not ('LyraTray.Session' -as [type])) {
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        $references = @('System.dll', 'System.Core.dll', 'System.Windows.Forms.dll', 'System.Drawing.dll',
            'System.Net.Http.dll', 'System.Web.Extensions.dll',
            [Windows.Automation.AutomationElement].Assembly.Location,
            [Windows.Automation.ControlType].Assembly.Location)
        Add-Type -Path @("$PSScriptRoot\windows\ProcessJob.cs", "$PSScriptRoot\windows\Tray.cs",
            "$PSScriptRoot\windows\BrowserTab.cs") -ReferencedAssemblies $references
    }
    [LyraTray.Session]::Run($PSScriptRoot, $cargo, $ollama, $Model, $Port, $WorkerDirectory, $BuildDirectory)
} catch {
    [Console]::WriteLine($_.Exception.GetBaseException().Message)
    if ($CloseStartupConsole) { [void](Read-Host 'Press Enter to close') }
    exit 1
} finally {
    if ($logWriter) { $logWriter.Dispose() }
}
