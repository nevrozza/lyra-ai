param(
    [string]$Model = '',
    [ValidateRange(1, 65535)]
    [int]$Port = 8787
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

function Find-Application($Names, $Fallbacks) {
    $command = Get-Command -Name $Names -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) { return $command.Source }
    foreach ($path in $Fallbacks) {
        if (Test-Path -LiteralPath $path -PathType Leaf) { return $path }
    }
    throw "Cannot find $($Names -join ' or '). Install it and reopen Windows Terminal or sign in again to refresh PATH."
}

try {
    $cargo = Find-Application @('cargo.exe') @("$env:USERPROFILE\.cargo\bin\cargo.exe")
    $ollama = Find-Application @('ollama.exe') @("$env:LOCALAPPDATA\Programs\Ollama\ollama.exe")
    $pi = Find-Application @('pi.exe', 'pi.cmd') @("$env:USERPROFILE\.pi\agent\bin\pi.cmd", "$env:LOCALAPPDATA\pi-node\current\pi.cmd", "$env:APPDATA\npm\pi.cmd")
    $env:PATH = "$(Split-Path -Parent $pi);$(Split-Path -Parent $cargo);$env:PATH"
    if (-not ('LyraTray.Session' -as [type])) {
        Add-Type -Path @("$PSScriptRoot\windows\ProcessJob.cs", "$PSScriptRoot\windows\Tray.cs") `
            -ReferencedAssemblies 'System.dll', 'System.Core.dll', 'System.Windows.Forms.dll', 'System.Drawing.dll', 'System.Net.Http.dll', 'System.Web.Extensions.dll'
    }
    [LyraTray.Session]::Run($PSScriptRoot, $cargo, $ollama, $Model, $Port)
} catch {
    [void][System.Windows.Forms.MessageBox]::Show($_.Exception.Message, 'Lyra AI', 'OK', 'Error')
    exit 1
}
