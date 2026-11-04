$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

function Invoke-Yarn {
    param(
        [Parameter(ValueFromRemainingArguments = $true)]
        [string[]]$Args
    )

    if (Get-Command yarn -ErrorAction SilentlyContinue) {
        & yarn @Args
        if ($LASTEXITCODE -ne 0) { throw "yarn failed with exit code $LASTEXITCODE" }
        return
    }

    & corepack yarn @Args
    if ($LASTEXITCODE -ne 0) { throw "yarn failed with exit code $LASTEXITCODE" }
}

function Rename-And-MoveBundle {
    param(
        [string]$BundlePath,
        [string]$Suffix,
        [string]$ProductName,
        [switch]$Debug
    )

    if (-not (Test-Path $BundlePath)) {
        return
    }

    Get-ChildItem -Path $BundlePath -File | ForEach-Object {
        $newName = $_.Name

        if ($ProductName) {
            $newName = $newName -replace [regex]::Escape($ProductName), "LIVE-$Suffix"
        } elseif ($newName -notmatch "(?i)live-$([regex]::Escape($Suffix))") {
            $newName = $newName -replace 'LIVE', "LIVE-$Suffix"
        }

        if ($Debug) {
            $newName = $newName -replace '\.msi$', '-debug.msi'
            $newName = $newName -replace '\.exe$', '-debug.exe'
        }

        Rename-Item -Path $_.FullName -NewName $newName
        Move-Item -Path (Join-Path $_.DirectoryName $newName) -Destination ./src-tauri/target/ -Force
    }
}

function Test-CudaResources {
    $patterns = @("cudart64*.dll", "cublas64*.dll", "cublasLt64*.dll")
    foreach ($pattern in $patterns) {
        $found = Get-ChildItem -Path ./src-tauri -Filter $pattern -ErrorAction SilentlyContinue
        if (-not $found) {
            return $false
        }
    }
    return $true
}

function Test-ReleaseBundleOutputs {
    param(
        [string]$Suffix
    )

    $patterns = @(
        "LIVE-$Suffix*_x64-setup.exe",
        "LIVE-$Suffix*_x64_en-US.msi"
    )

    foreach ($pattern in $patterns) {
        $found = Get-ChildItem -Path ./src-tauri/target -Filter $pattern -ErrorAction SilentlyContinue
        if (-not $found) {
            return $false
        }
    }

    return $true
}

$requireCudaBuild = $env:REQUIRE_CUDA_BUILD -eq "1"

# CPU build
Invoke-Yarn tauri build --features gui --config src-tauri/tauri.windows.cpu.conf.json
Invoke-Yarn tauri build --debug --features gui --config src-tauri/tauri.windows.cpu.conf.json

Rename-And-MoveBundle -BundlePath ./src-tauri/target/release/bundle/msi -Suffix "cpu" -ProductName "LIVE"
Rename-And-MoveBundle -BundlePath ./src-tauri/target/release/bundle/nsis -Suffix "cpu" -ProductName "LIVE"
Rename-And-MoveBundle -BundlePath ./src-tauri/target/debug/bundle/msi -Suffix "cpu" -ProductName "LIVE" -Debug
Rename-And-MoveBundle -BundlePath ./src-tauri/target/debug/bundle/nsis -Suffix "cpu" -ProductName "LIVE" -Debug

# CUDA build (override config)
if (Test-CudaResources) {
    Invoke-Yarn tauri build --features gui,cuda --config src-tauri/tauri.windows.cuda.conf.json
    Invoke-Yarn tauri build --debug --features gui,cuda --config src-tauri/tauri.windows.cuda.conf.json
} else {
    if ($requireCudaBuild) {
        throw "CUDA build required, but cudart/cublas runtime DLLs are missing from ./src-tauri."
    }
    Write-Host "Skip CUDA build: missing cudart/cublas DLLs in ./src-tauri" -ForegroundColor Yellow
}

Rename-And-MoveBundle -BundlePath ./src-tauri/target/release/bundle/msi -Suffix "cuda" -ProductName "LIVE"
Rename-And-MoveBundle -BundlePath ./src-tauri/target/release/bundle/nsis -Suffix "cuda" -ProductName "LIVE"
Rename-And-MoveBundle -BundlePath ./src-tauri/target/debug/bundle/msi -Suffix "cuda" -ProductName "LIVE" -Debug
Rename-And-MoveBundle -BundlePath ./src-tauri/target/debug/bundle/nsis -Suffix "cuda" -ProductName "LIVE" -Debug

if ($requireCudaBuild -and -not (Test-ReleaseBundleOutputs -Suffix "cuda")) {
    throw "CUDA build was required, but release bundles were not produced in ./src-tauri/target."
}
