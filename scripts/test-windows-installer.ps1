param([Parameter(Mandatory = $true)][string]$Installer)
$ErrorActionPreference = 'Stop'
$installerPath = (Resolve-Path -LiteralPath $Installer).Path
$directory = Join-Path ([IO.Path]::GetTempPath()) ('LIVE-install-' + [guid]::NewGuid().ToString('N'))
$app = $null
$installProcess = $null
$previousPath = $env:PATH
try {
    $installProcess = Start-Process -FilePath $installerPath -ArgumentList @('/S', "/D=$directory") -PassThru
    if (-not $installProcess.WaitForExit(180000)) { throw 'Installer timed out after 180 seconds.' }
    if ($installProcess.ExitCode -ne 0) { throw "Installer exited with code $($installProcess.ExitCode)." }
    $binary = Join-Path $directory 'LIVE.exe'
    if (-not (Test-Path -LiteralPath $binary)) { throw 'Installer did not create LIVE.exe.' }
    $env:PATH = (($env:PATH -split ';') | Where-Object { $_ -notmatch '(?i)CUDA|NVIDIA GPU Computing Toolkit' }) -join ';'
    $app = Start-Process -FilePath $binary -WorkingDirectory $directory -PassThru
    Start-Sleep -Seconds 20
    $app.Refresh()
    if ($app.HasExited) { throw "Installed LIVE.exe exited during startup with code $($app.ExitCode)." }
    Write-Host "Installed application startup PASS: $([IO.Path]::GetFileName($installerPath))"
} finally {
    if ($app -and -not $app.HasExited) {
        Stop-Process -Id $app.Id -Force
        # Stop-Process returns early; the running app still locks files in the folder.
        $null = $app.WaitForExit(30000)
    }
    if ($installProcess -and -not $installProcess.HasExited) { Stop-Process -Id $installProcess.Id -Force }
    $env:PATH = $previousPath
    $uninstaller = Join-Path $directory 'uninstall.exe'
    if (Test-Path -LiteralPath $uninstaller) {
        $uninstall = Start-Process -FilePath $uninstaller -ArgumentList '/S' -PassThru
        if (-not $uninstall.WaitForExit(60000)) { Stop-Process -Id $uninstall.Id -Force }
    }
    if (Test-Path -LiteralPath $directory) { Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue }
}
