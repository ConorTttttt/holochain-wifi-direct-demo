# Add or remove the scoped inbound rules hc-wfd needs, by hand.
#
# hc-wfd create adds these itself when run elevated, and removes them on
# exit. Use this script when running hc-wfd unelevated, or to clean up after
# an interrupted run.
#
# Usage (elevated PowerShell):
#   scripts\windows-firewall.ps1 -Add -LocalIp <group adapter address> [-UdpPort 47001] [-TcpPort 47002] [-Exe C:\hc-wifi-target\release\hc-wfd.exe]
#   scripts\windows-firewall.ps1 -Remove
#   scripts\windows-firewall.ps1 -RemoveAll     (also rules left by hc-wfd create, named hc-wfd-<session>)
#
# The rules allow only: inbound to -Exe, on -LocalIp, UDP -UdpPort (Holochain)
# and TCP -TcpPort (pairing), from the local subnet, on wireless interfaces.
# Nothing else in the firewall changes.
param(
    [switch]$Add,
    [switch]$Remove,
    [switch]$RemoveAll,
    [string]$LocalIp = "",
    [int]$UdpPort = 47001,
    [int]$TcpPort = 47002,
    [string]$Exe = "C:\hc-wifi-target\release\hc-wfd.exe"
)
$ErrorActionPreference = "Stop"
$name = "hc-wfd-manual"
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { Write-Error "Run this in an elevated (Run as administrator) PowerShell" }

if ($Add) {
    if (-not $LocalIp) { Write-Error "-Add needs -LocalIp: the address hc-wfd create printed for the group adapter" }
    if (-not (Test-Path $Exe)) { Write-Error "$Exe not found" }
    foreach ($rule in @(@{ P = "UDP"; Port = $UdpPort }, @{ P = "TCP"; Port = $TcpPort })) {
        New-NetFirewallRule -DisplayName $name -Direction Inbound -Action Allow -Program $Exe `
            -Protocol $rule.P -LocalPort $rule.Port -LocalAddress $LocalIp -RemoteAddress LocalSubnet `
            -InterfaceType Wireless -Profile Any | Out-Null
        Write-Host "added     ${name}: $($rule.P) $($rule.Port) to $LocalIp, from the local subnet, wireless, $Exe only"
    }
    Write-Host "Remove with: scripts\windows-firewall.ps1 -Remove"
} elseif ($Remove -or $RemoveAll) {
    $pattern = if ($RemoveAll) { "hc-wfd*" } else { $name }
    $rules = Get-NetFirewallRule -DisplayName $pattern -ErrorAction SilentlyContinue
    if (-not $rules) { Write-Host "no $pattern rules" }
    foreach ($r in $rules) { Remove-NetFirewallRule -Name $r.Name; Write-Host "removed   $($r.DisplayName)" }
} else {
    Get-Content $PSCommandPath | Select-Object -Skip 1 -First 16
    exit 2
}
