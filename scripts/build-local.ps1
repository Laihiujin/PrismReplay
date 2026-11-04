param([ValidateSet('debug', 'release')][string]$Profile = 'debug')
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location -LiteralPath $root

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (-not (Test-Path -LiteralPath (Join-Path $cargoBin 'cargo.exe'))) { throw 'Rust is not installed.' }
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Visual Studio C++ Build Tools are not installed.' }
$vs = & $vswhere -all -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'MSVC and Windows SDK are required.' }
$cmakeBin = Join-Path $vs 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin'
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
$devEnvironment = & cmd.exe /d /c "call `"$vcvars`" >nul && set"
if ($LASTEXITCODE -ne 0) { throw 'Unable to initialize the MSVC x64 environment.' }
foreach ($line in $devEnvironment) {
    if ($line -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process') }
}
$ninjaBin = Join-Path $vs 'Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja'
$env:PATH = "$cargoBin;$cmakeBin;$ninjaBin;$(Join-Path $root 'src-tauri');$env:PATH"
# Only load a DLL for this x64 build host; the VS component also ships ARM64 DLLs.
$clangCandidates = @(
    $(if ($env:LIBCLANG_PATH) { Join-Path $env:LIBCLANG_PATH 'libclang.dll' }),
    (Join-Path (Split-Path $root -Parent) 'python-libclang\clang\native\libclang.dll'),
    (Join-Path $vs 'VC\Tools\Llvm\x64\bin\libclang.dll'),
    (Join-Path $vs 'VC\Tools\Llvm\bin\libclang.dll')
)
$libclang = $null
foreach ($candidate in $clangCandidates) {
    if (-not $candidate -or -not (Test-Path -LiteralPath $candidate)) { continue }
    $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($candidate))
    try {
        $reader.BaseStream.Position = 0x3c
        $peOffset = $reader.ReadInt32()
        $reader.BaseStream.Position = $peOffset + 4
        if ($reader.ReadUInt16() -eq 0x8664) { $libclang = Get-Item -LiteralPath $candidate; break }
    } finally { $reader.Dispose() }
}
if (-not $libclang) { throw 'An x64 libclang.dll is required. Set LIBCLANG_PATH to its directory.' }
$env:LIBCLANG_PATH = $libclang.DirectoryName
$env:CARGO_NET_GIT_FETCH_WITH_CLI = 'true'
# The bundled whisper-rs-sys bindings describe Linux FILE layouts; generate them for the MSVC target instead.
Remove-Item Env:WHISPER_DONT_GENERATE_BINDINGS -ErrorAction SilentlyContinue
$env:CMAKE_GENERATOR = 'Ninja'
# Discard only an incompatible generated cache left by an earlier VS generator.
Get-ChildItem -Path (Join-Path $root "src-tauri\target\$Profile\build\whisper-rs-sys-*\out\build\CMakeCache.txt") -ErrorAction SilentlyContinue | ForEach-Object {
    if (Select-String -LiteralPath $_.FullName -Pattern '^CMAKE_GENERATOR:INTERNAL=Visual Studio' -Quiet) {
        Remove-Item -LiteralPath $_.FullName
    }
}
foreach ($name in @('ffmpeg.exe', 'ffprobe.exe', 'ffplay.exe')) {
    if (-not (Test-Path -LiteralPath (Join-Path $root "src-tauri\$name"))) {
        throw "Missing media resource src-tauri\$name. Run ffmpeg_setup.ps1 in this checkout first."
    }
}
& corepack yarn install --frozen-lockfile
if ($LASTEXITCODE -ne 0) { throw "Dependency installation failed ($LASTEXITCODE)." }
$buildArgs = @('build', '--features', 'gui,isolated-build', '--config', 'src-tauri/tauri.local.conf.json', '--no-bundle')
if ($Profile -eq 'debug') { $buildArgs += '--debug' }
$buildArgs += @('--', '--locked')
# Invoke the local CLI directly: Yarn 1 consumes arguments before a literal --.
& (Join-Path $root 'node_modules\.bin\tauri.cmd') @buildArgs
if ($LASTEXITCODE -ne 0) { throw "Isolated native build failed ($LASTEXITCODE)." }

$output = Join-Path $root 'src-tauri\target\local-artifacts'
New-Item -ItemType Directory -Path $output -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $root "src-tauri\target\$Profile\shadowverse.exe") -Destination (Join-Path $output 'PrismReplay-Local.exe')
foreach ($name in @('ffmpeg.exe', 'ffprobe.exe', 'ffplay.exe')) {
    Copy-Item -LiteralPath (Join-Path $root "src-tauri\$name") -Destination $output
}
@'
@echo off
setlocal
cd /d "%~dp0"
set "PATH=%~dp0;%PATH%"
start "" "%~dp0PrismReplay-Local.exe"
'@ | Set-Content -LiteralPath (Join-Path $output 'Launch-Local.cmd') -Encoding Ascii
Get-FileHash -LiteralPath (Join-Path $output 'PrismReplay-Local.exe') -Algorithm SHA256
Write-Output "Isolated local build: $output"
Write-Output 'No installer was run. No installed application or existing account/cache directory was modified.'
