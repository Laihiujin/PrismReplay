param(
    [Parameter(Mandatory = $true)][string]$Binary,
    [Parameter(Mandatory = $true)][string]$CudaRuntime
)
$ErrorActionPreference = 'Stop'
$directory = Join-Path ([IO.Path]::GetTempPath()) ('LIVE-startup-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $directory | Out-Null
$app = $null
$previousPath = $env:PATH
try {
    Copy-Item -LiteralPath $Binary -Destination (Join-Path $directory 'LIVE.exe')
    Copy-Item -LiteralPath $CudaRuntime -Destination $directory
    # Do not accidentally satisfy delay imports from the build toolkit PATH.
    $env:PATH = (($env:PATH -split ';') | Where-Object { $_ -notmatch '(?i)CUDA|NVIDIA GPU Computing Toolkit' }) -join ';'
    foreach ($dll in @('cublas64_12.dll', 'cublasLt64_12.dll', 'nvcuda.dll')) {
        if (Test-Path (Join-Path $directory $dll)) { throw "Unexpected GPU library in isolated startup folder: $dll" }
    }
    $app = Start-Process (Join-Path $directory 'LIVE.exe') -WorkingDirectory $directory -PassThru
    Start-Sleep -Seconds 20
    $app.Refresh()
    if ($app.HasExited) { throw "LIVE.exe exited during isolated startup with code $($app.ExitCode)." }
    Write-Host 'LIVE CUDA startup PASS: process remained alive without bundled cublas or CUDA toolkit PATH.'
} finally {
    if ($app -and -not $app.HasExited) { Stop-Process -Id $app.Id -Force }
    $env:PATH = $previousPath
    Remove-Item -LiteralPath $directory -Recurse -Force
}
