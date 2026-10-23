# Build hc-wfd.exe on Windows, exactly as Cargo.lock pins it.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File C:\irohble\holochain-wifi-direct-demo\scripts\windows-build.ps1 [-TargetDir C:\hc-wifi-target] [-SkipCheck]
#
# Output: C:\hc-wifi-target\release\hc-wfd.exe. The BLE project's
# C:\hc-ble-target is never written to. The build fetches the pinned kitsune2
# commit from GitHub, so it needs internet; the first one takes 20 to 40
# minutes.
#
# The build output path must not contain spaces, or libsodium's build fails.
param(
    [string]$TargetDir = "C:\hc-wifi-target",
    [switch]$SkipCheck
)
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot

if ($TargetDir.Contains(" ")) { Write-Error "TargetDir must not contain spaces: $TargetDir" }
if ($TargetDir.TrimEnd("\") -ieq "C:\hc-ble-target") { Write-Error "C:\hc-ble-target belongs to the BLE build; use another -TargetDir" }

if (-not $SkipCheck) {
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "windows-check.ps1") -TargetDir $TargetDir
    if ($LASTEXITCODE -ne 0) { Write-Error "Prerequisite check failed. Fix the FAIL lines above, or rerun with -SkipCheck." }
}

# OpenSSL's build needs a native Windows perl; Git for Windows' msys perl,
# often first on PATH, breaks it.
$strawberry = "C:\Strawberry\perl\bin\perl.exe"
$perl = Get-Command perl -ErrorAction SilentlyContinue
$perlOs = $null
if ($perl) { $perlOs = & $perl.Source -e 'print $^O' 2>$null }
if ($perlOs -ne "MSWin32" -and (Test-Path $strawberry)) {
    $env:OPENSSL_SRC_PERL = $strawberry
    $env:PERL = $strawberry
    Write-Host "Using $strawberry for the OpenSSL build"
}
# The NASM installer does not add itself to PATH.
$nasmDir = Join-Path $env:ProgramFiles "NASM"
if (-not (Get-Command nasm -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $nasmDir "nasm.exe"))) {
    $env:PATH = "$nasmDir;$env:PATH"
    Write-Host "Added $nasmDir to PATH"
}

$env:CARGO_TARGET_DIR = $TargetDir
Push-Location $repo
try {
    Write-Host "cargo build --release --locked -p hc-wfd"
    & cargo build --release --locked -p hc-wfd
    if ($LASTEXITCODE -ne 0) {
        Write-Host ""
        Write-Host "Build failed. Common causes:" -ForegroundColor Red
        Write-Host "  openssl-src: perl is not Strawberry Perl, or nmake/cl not found (rerun windows-check.ps1)"
        Write-Host "  aws-lc-sys: NASM missing; install it, or set AWS_LC_SYS_PREBUILT_NASM=1"
        Write-Host "  libsodium-sys-stable: download.libsodium.org unreachable; set SODIUM_LIB_DIR to a libsodium build"
        Write-Host "  windows crate errors in crates\wifi-direct: report them; that code was type-checked on Linux only"
        Write-Host "  --locked: Cargo.lock out of date; do not update it on this machine, report it instead"
        exit $LASTEXITCODE
    }
} finally {
    Pop-Location
}

$exe = Join-Path $TargetDir "release\hc-wfd.exe"
if (-not (Test-Path $exe)) { Write-Error "Build reported success but $exe is missing" }
Write-Host ""
Write-Host "Built $exe ($([math]::Round((Get-Item $exe).Length / 1MB)) MB)" -ForegroundColor Green
& $exe --version
if ($LASTEXITCODE -ne 0) { Write-Error "$exe failed to start" }
Write-Host "Next: $exe probe     (starts a Wi-Fi Direct group for two seconds)"
