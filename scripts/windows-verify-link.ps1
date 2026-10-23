# Show, independently of hc-wfd's own report, that Holochain traffic to the
# Ubuntu peer crosses the Wi-Fi Direct adapter.
#
# Usage, while hc-wfd create is exchanging notes (elevated, for pktmon):
#   powershell -ExecutionPolicy Bypass -File scripts\windows-verify-link.ps1 -PeerIp <Ubuntu link address> [-Port 47001] [-Seconds 30]
#
# Checks the route Windows picks for PeerIp (a Wi-Fi Direct virtual adapter,
# no gateway) and that hc-wfd's only UDP socket is bound on that adapter.
# Then captures UDP -Port with pktmon for -Seconds, prints packet counters
# per adapter, and saves wfd-capture-<time>.pcapng for Wireshark (filter
# udp.port == 47001; the payload is encrypted QUIC).
param(
    [Parameter(Mandatory = $true)][string]$PeerIp,
    [int]$Port = 47001,
    [int]$Seconds = 30
)

$script:failures = 0
function Pass([string]$msg) { Write-Host "PASS  $msg" -ForegroundColor Green }
function Warn([string]$msg) { Write-Host "WARN  $msg" -ForegroundColor Yellow }
function Fail([string]$msg) { Write-Host "FAIL  $msg" -ForegroundColor Red; $script:failures++ }

Write-Host "== Route to $PeerIp"
$route = Find-NetRoute -RemoteIPAddress $PeerIp -ErrorAction SilentlyContinue
$source = $route | Where-Object { $_.CimClass.CimClassName -eq "MSFT_NetIPAddress" } | Select-Object -First 1
$hop = $route | Where-Object { $_.CimClass.CimClassName -eq "MSFT_NetRoute" } | Select-Object -First 1
if (-not $source) {
    Fail "no route to $PeerIp"
} else {
    $adapter = Get-NetAdapter -IncludeHidden -InterfaceIndex $source.InterfaceIndex
    Write-Host "      via '$($adapter.Name)' ($($adapter.InterfaceDescription)), from $($source.IPAddress), next hop $($hop.NextHop)"
    if ($hop.NextHop -and $hop.NextHop -ne "0.0.0.0") { Fail "$PeerIp is reached through gateway $($hop.NextHop), not directly" }
    elseif ($adapter.InterfaceDescription -like "*Wi-Fi Direct*") { Pass "$PeerIp is on the Wi-Fi Direct adapter '$($adapter.Name)'" }
    else { Fail "$PeerIp is reached through '$($adapter.InterfaceDescription)', which is not a Wi-Fi Direct adapter" }
}

Write-Host "== hc-wfd sockets"
$proc = Get-Process hc-wfd -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) {
    Fail "hc-wfd is not running"
} else {
    $udp = @(Get-NetUDPEndpoint -OwningProcess $proc.Id -ErrorAction SilentlyContinue)
    $udp | ForEach-Object { Write-Host "      UDP $($_.LocalAddress):$($_.LocalPort)" }
    if ($udp.Count -eq 1 -and $source -and $udp[0].LocalAddress -eq $source.IPAddress -and $udp[0].LocalPort -eq $Port) {
        Pass "hc-wfd's only UDP socket is bound to $($source.IPAddress):$Port on the Wi-Fi Direct adapter"
    } else {
        Fail "expected exactly one hc-wfd UDP socket, bound to $($source.IPAddress):$Port"
    }
    Get-NetTCPConnection -OwningProcess $proc.Id -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "      TCP $($_.LocalAddress):$($_.LocalPort) -> $($_.RemoteAddress):$($_.RemotePort) $($_.State)" }
}

Write-Host "== Capture of UDP port $Port for ${Seconds}s (pktmon)"
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Warn "pktmon needs an elevated prompt; rerun as administrator for the capture"
} else {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $etl = Join-Path (Get-Location) "wfd-capture-$stamp.etl"
    pktmon filter remove | Out-Null
    pktmon filter add hc-wfd -t UDP -p $Port | Out-Null
    pktmon start --capture --comp nics --pkt-size 128 --file-name $etl | Out-Null
    Start-Sleep -Seconds $Seconds
    Write-Host "      packet counters per adapter (the Wi-Fi Direct adapter should carry them all):"
    pktmon counters | ForEach-Object { "        $_" }
    pktmon stop | Out-Null
    pktmon filter remove | Out-Null
    $pcap = [System.IO.Path]::ChangeExtension($etl, ".pcapng")
    pktmon etl2pcap $etl --out $pcap | Out-Null
    Remove-Item $etl -ErrorAction SilentlyContinue
    if (Test-Path $pcap) { Pass "capture saved to $pcap" } else { Warn "pktmon did not produce $pcap" }
}

Write-Host ""
if ($script:failures -gt 0) { Write-Host "RESULT  FAIL ($script:failures failed)" -ForegroundColor Red; exit 1 }
Write-Host "RESULT  PASS" -ForegroundColor Green
exit 0
