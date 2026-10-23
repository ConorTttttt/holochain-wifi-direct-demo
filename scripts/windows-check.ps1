# Check that this Windows PC can build hc-wfd and host a Wi-Fi Direct group.
# Changes nothing.
#
# Usage (Windows PowerShell 5.1 or PowerShell 7):
#   powershell -ExecutionPolicy Bypass -File C:\irohble\holochain-wifi-direct-demo\scripts\windows-check.ps1 [-TargetDir C:\hc-wifi-target] [-SkipBuildTools]
#
# Prints one PASS, WARN, FAIL or INFO line per check and exits 1 if anything
# FAILed. The definitive Wi-Fi Direct test is `hc-wfd.exe probe`, which starts
# a group for two seconds; this script reports what Windows says beforehand.
param(
    [string]$TargetDir = "C:\hc-wifi-target",
    [switch]$SkipBuildTools
)

$script:failures = 0
function Pass([string]$msg) { Write-Host "PASS  $msg" -ForegroundColor Green }
function Warn([string]$msg) { Write-Host "WARN  $msg" -ForegroundColor Yellow }
function Fail([string]$msg) { Write-Host "FAIL  $msg" -ForegroundColor Red; $script:failures++ }
function Info([string]$msg) { Write-Host "INFO  $msg" }
$repo = Split-Path -Parent $PSScriptRoot

Write-Host "== System"
$os = [System.Environment]::OSVersion.Version
if ($os.Major -ge 10) { Pass "Windows $os" } else { Fail "Windows 10 or later is required, found $os" }
if ([Environment]::Is64BitOperatingSystem) { Pass "64-bit Windows" } else { Fail "64-bit Windows is required" }
if ($repo.Contains(" ")) { Fail "The repository path must not contain spaces: $repo" } else { Pass "Repository at $repo" }

if (-not $SkipBuildTools) {
    Write-Host "== Rust"
    if (Get-Command cargo -ErrorAction SilentlyContinue) {
        $hostLine = (rustc -vV 2>$null | Select-String "^host:").ToString()
        if ($hostLine -match "x86_64-pc-windows-msvc") { Pass "Rust host is x86_64-pc-windows-msvc" }
        else { Fail "Rust host must be x86_64-pc-windows-msvc, found '$hostLine'. Run: rustup set default-host x86_64-pc-windows-msvc" }
        $channel = (Select-String -Path (Join-Path $repo "rust-toolchain.toml") -Pattern 'channel\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
        if (((rustup toolchain list 2>$null) -join "`n") -match [regex]::Escape($channel)) { Pass "Rust $channel installed" }
        else { Warn "Rust $channel is not installed yet; rustup installs it on the first build" }
    } else {
        Fail "cargo not found. Install Rust from https://rustup.rs with the MSVC host, then open a new terminal"
    }

    Write-Host "== MSVC, Windows SDK, Perl, NASM, Git"
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    $vsPath = $null
    if (Test-Path $vswhere) { $vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath }
    if ($vsPath) { Pass "MSVC C++ x64 build tools: $vsPath" } else { Fail "MSVC C++ build tools not found. Install Visual Studio Build Tools with 'Desktop development with C++'" }
    $sdkInclude = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\Include"
    if ((Test-Path $sdkInclude) -and (Get-ChildItem $sdkInclude -Directory -ErrorAction SilentlyContinue)) { Pass "Windows SDK $((Get-ChildItem $sdkInclude -Directory | Sort-Object Name | Select-Object -Last 1).Name)" }
    else { Fail "Windows SDK not found. Add a Windows 10 or 11 SDK in the Visual Studio Installer" }
    $strawberry = "C:\Strawberry\perl\bin\perl.exe"
    $perl = Get-Command perl -ErrorAction SilentlyContinue
    $perlOs = $null
    if ($perl) { $perlOs = & $perl.Source -e 'print $^O' 2>$null }
    if ($perlOs -eq "MSWin32") { Pass "Native Windows perl: $($perl.Source)" }
    elseif (Test-Path $strawberry) { Warn "perl on PATH is not native Windows perl; windows-build.ps1 uses $strawberry" }
    else { Fail "Strawberry Perl not found (OpenSSL's build for lair needs it): https://strawberryperl.com" }
    if (Get-Command nasm -ErrorAction SilentlyContinue) { Pass "nasm on PATH" }
    elseif (Test-Path (Join-Path $env:ProgramFiles "NASM\nasm.exe")) { Warn "NASM is installed but not on PATH; windows-build.ps1 adds it" }
    else { Fail "NASM not found: winget install NASM.NASM" }
    if (Get-Command git -ErrorAction SilentlyContinue) { Pass ((git --version) -join " ") } else { Fail "git not found" }

    Write-Host "== Build output"
    if ($TargetDir.Contains(" ")) { Fail "TargetDir must not contain spaces (libsodium fails): $TargetDir" } else { Pass "TargetDir $TargetDir (the BLE build's C:\hc-ble-target is not touched)" }
    $drive = Get-PSDrive -Name ($TargetDir.Substring(0, 1)) -ErrorAction SilentlyContinue
    if ($drive) {
        $freeGb = [math]::Round($drive.Free / 1GB)
        if ($freeGb -ge 15) { Pass "$freeGb GB free on $($drive.Name):" } else { Warn "Only $freeGb GB free on $($drive.Name):; a release build needs about 10 GB" }
    }
}

Write-Host "== Wi-Fi adapter and driver"
$wifi = Get-NetAdapter -Physical -ErrorAction SilentlyContinue | Where-Object { $_.PhysicalMediaType -eq "Native 802.11" -or $_.InterfaceDescription -match "Wi-?Fi|Wireless|802\.11" }
if ($wifi) { foreach ($a in $wifi) { Pass "Wi-Fi adapter '$($a.Name)': $($a.InterfaceDescription), $($a.Status)" } } else { Fail "No Wi-Fi adapter found" }
$drivers = (netsh wlan show drivers) -join "`n"
if ($drivers -match "Wireless Display Supported\s*:\s*(.+)") { Info "Wireless Display Supported: $($Matches[1].Trim())" }
$wfd = (netsh wlan show wirelesscapabilities 2>$null) -join "`n"
foreach ($cap in "WiFi Direct Device", "WiFi Direct GO", "WiFi Direct Client") {
    if ($wfd -match "$cap\s*:\s*Supported") { Pass "$cap supported" }
    elseif ($wfd -match "$cap\s*:\s*(\S+)") { if ($cap -eq "WiFi Direct Client") { Info "$cap $($Matches[1])" } else { Fail "$cap $($Matches[1])" } }
}
if ($wfd -match "Number of Wi-Fi Direct GO.*:\s*(\d+)") { Info "Wi-Fi Direct GO ports: $($Matches[1])" }
$wfdAdapters = Get-NetAdapter -IncludeHidden -ErrorAction SilentlyContinue | Where-Object { $_.InterfaceDescription -like "*Wi-Fi Direct*" }
if ($wfdAdapters) { foreach ($a in $wfdAdapters) { Info "$($a.InterfaceDescription): $($a.Status) (Disconnected is normal with no group running)" } }
else { Warn "No 'Microsoft Wi-Fi Direct Virtual Adapter' found" }

Write-Host "== Mobile Hotspot (it and a Wi-Fi Direct group owner can not run together)"
try {
    $null = [Windows.Networking.Connectivity.NetworkInformation, Windows.Networking.Connectivity, ContentType = WindowsRuntime]
    $null = [Windows.Networking.NetworkOperators.NetworkOperatorTetheringManager, Windows.Networking.NetworkOperators, ContentType = WindowsRuntime]
    $connectionProfile = [Windows.Networking.Connectivity.NetworkInformation]::GetInternetConnectionProfile()
    if ($connectionProfile) {
        $state = [Windows.Networking.NetworkOperators.NetworkOperatorTetheringManager]::CreateFromConnectionProfile($connectionProfile).TetheringOperationalState
        if ("$state" -eq "On" -or "$state" -eq "InTransition") { Fail "Mobile Hotspot is $state. Turn it off in Settings > Network & internet > Mobile hotspot" }
        else { Pass "Mobile Hotspot is $state" }
    } else { Pass "No internet connection profile, so Mobile Hotspot can not be on" }
} catch {
    Warn "Could not read the Mobile Hotspot state (needs Windows PowerShell 5.1); make sure it is off"
}

Write-Host "== Firewall"
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($isAdmin) { Pass "Elevated: hc-wfd create can add its scoped firewall rules itself" }
else { Info "Not elevated: run hc-wfd create from an elevated terminal for scoped firewall rules, or allow it when Windows asks" }
$exe = Join-Path $TargetDir "release\hc-wfd.exe"
$blocks = Get-NetFirewallApplicationFilter -Program $exe -ErrorAction SilentlyContinue | Get-NetFirewallRule -ErrorAction SilentlyContinue | Where-Object { $_.Action -eq "Block" -and $_.Enabled -eq "True" }
if ($blocks) { Fail "Block rules exist for $exe (from clicking Cancel on Windows' prompt); they override allow rules. Remove them: $(($blocks | ForEach-Object { $_.DisplayName }) -join ', ')" }
$stale = Get-NetFirewallRule -DisplayName "hc-wfd-*" -ErrorAction SilentlyContinue
if ($stale) { Warn "Leftover hc-wfd rules from an interrupted run: $(($stale | ForEach-Object { $_.DisplayName }) -join ', '). Remove with scripts\windows-firewall.ps1 -RemoveAll" }

Write-Host "== hc-wfd"
if (Test-Path $exe) {
    Pass "$exe exists. For the definitive group owner test run: $exe probe"
} else {
    Info "$exe not built yet: run scripts\windows-build.ps1"
}

Write-Host ""
if ($script:failures -gt 0) { Write-Host "RESULT  FAIL ($script:failures failed)" -ForegroundColor Red; exit 1 }
Write-Host "RESULT  PASS" -ForegroundColor Green
exit 0
