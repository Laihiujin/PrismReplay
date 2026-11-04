param([string]$Source = (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent))
$ErrorActionPreference = 'Stop'
. (Join-Path $Source 'scripts/verify-cuda-imports.ps1')
$fixtures = @(
    @{ Name = 'unused cublasLt is valid'; Regular = @('kernel32.dll', 'cudart64_12.dll'); Delayed = @('cublas64_12.dll', 'nvcuda.dll'); Pass = $true },
    @{ Name = 'both math libraries delayed'; Regular = @('kernel32.dll'); Delayed = @('cublas64_12.dll', 'cublasLt64_12.dll', 'nvcuda.dll'); Pass = $true },
    @{ Name = 'eager cublas rejected'; Regular = @('cublas64_12.dll'); Delayed = @('nvcuda.dll'); Pass = $false },
    @{ Name = 'eager cublasLt rejected'; Regular = @('cublasLt64_12.dll'); Delayed = @('cublas64_12.dll'); Pass = $false },
    @{ Name = 'eager driver rejected'; Regular = @('nvcuda.dll'); Delayed = @('cublas64_12.dll'); Pass = $false },
    @{ Name = 'missing cublas rejected'; Regular = @('kernel32.dll'); Delayed = @('nvcuda.dll'); Pass = $false }
)
foreach ($fixture in $fixtures) {
    $text = "Image has the following dependencies:`r`n" + (($fixture.Regular | ForEach-Object { "    $_" }) -join "`r`n")
    $text += "`r`nImage has the following delay load dependencies:`r`n" + (($fixture.Delayed | ForEach-Object { "    $_" }) -join "`r`n") + "`r`nSummary`r`n"
    $passed = $true
    try { $null = Assert-CudaDelayImports -Dependents $text } catch { $passed = $false }
    if ($passed -ne $fixture.Pass) { throw "FAIL: $($fixture.Name)" }
    Write-Host "PASS: $($fixture.Name)"
}
