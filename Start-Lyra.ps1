param(
    [string]$Model = '',
    [ValidateRange(1, 65535)]
    [int]$Port = 8787,
    [switch]$CloseStartupConsole
)

$ErrorActionPreference = 'Stop'
Write-Host 'Lyra AI startup'

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

function Find-Application($Names, $Fallbacks) {
    $command = Get-Command -Name $Names -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) { return $command.Source }
    foreach ($path in $Fallbacks) {
        if (Test-Path -LiteralPath $path -PathType Leaf) { return $path }
    }
    throw "Cannot find $($Names -join ' or '). Install it and reopen Windows Terminal or sign in again to refresh PATH."
}

try {
    try { Update-LyraShortcut }
    catch { Write-Warning "Could not update the Lyra AI shortcut: $($_.Exception.Message)" }
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    Write-Host 'Checking Cargo, Ollama and Pi...'
    $cargo = Find-Application @('cargo.exe') @("$env:USERPROFILE\.cargo\bin\cargo.exe")
    $ollama = Find-Application @('ollama.exe') @("$env:LOCALAPPDATA\Programs\Ollama\ollama.exe")
    $pi = Find-Application @('pi.exe', 'pi.cmd') @("$env:USERPROFILE\.pi\agent\bin\pi.cmd", "$env:LOCALAPPDATA\pi-node\current\pi.cmd", "$env:APPDATA\npm\pi.cmd")
    $env:PATH = "$(Split-Path -Parent $pi);$(Split-Path -Parent $cargo);$env:PATH"
    if (-not ('LyraTray.Session' -as [type])) {
        Add-Type -Path @("$PSScriptRoot\windows\ProcessJob.cs", "$PSScriptRoot\windows\Tray.cs") `
            -ReferencedAssemblies 'System.dll', 'System.Core.dll', 'System.Windows.Forms.dll', 'System.Drawing.dll', 'System.Net.Http.dll', 'System.Web.Extensions.dll'
    }
    [LyraTray.Session]::Run($PSScriptRoot, $cargo, $ollama, $Model, $Port, $CloseStartupConsole.IsPresent)
} catch {
    Write-Host ($_.Exception.GetBaseException().Message) -ForegroundColor Red
    if ($CloseStartupConsole) { [void](Read-Host 'Press Enter to close') }
    exit 1
}
