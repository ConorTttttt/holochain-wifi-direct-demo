# Later

Not built. The two-desktop prototype comes first.

- **Native P2P on Ubuntu (mode A).** Ubuntu negotiates as a Wi-Fi Direct
  client with `wpa_supplicant` (`p2p_find`, `p2p_connect <addr> pbc`) instead
  of joining as a legacy station. It would use the P2P-client slot rather
  than the station slot, so home Wi-Fi would stay up. Windows has to accept
  through `WiFiDirectConnectionListener` and `WiFiDirectDevice.FromIdAsync`,
  with `DeviceInformationCustomPairing` for WPS. hc-wfd already logs such
  requests. The QR code would carry the GO's device address and a WPS PIN.
- **Android.** Scan the QR code with the camera, join the group with
  `WifiNetworkSpecifier` (or as a P2P client with `WifiP2pManager`), and run
  the pairing channel. Holochain on Android is its own project.
- **iOS.** iOS can not join Wi-Fi Direct groups; it would need
  `NEHotspotConfiguration` against a legacy-mode group, which is what this
  design offers, plus a Holochain runtime.
- **BLE-assisted discovery.** Advertise the invitation's session ID over BLE,
  using the BLE project's stack, so a nearby device can find the group
  without a QR code and fetch the invitation over an authenticated GATT
  exchange.
- **Choosing between BLE and Wi-Fi Direct.** One conductor, one iroh
  endpoint, with both the BLE custom transport and a direct IP socket. That
  needs kitsune2 to advertise and dial more than one URL per agent.
- **Reconnecting to trusted peers.** Remember a paired peer's agent key, and
  re-pair with a long-lived secret rather than a new QR code. The DNA would
  then need a stable network seed.
- **More than one client.** A group owner can take several legacy clients.
  The pairing service would serve one invitation per client, or one
  multi-use invitation bounded by count.
- **A graphical app.** The terminal flow already has the stages a GUI needs:
  a window showing the QR code on Windows, and a camera view on the joining
  device.
