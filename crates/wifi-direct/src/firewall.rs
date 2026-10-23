//! Windows Firewall rules that let the paired peer in, and nothing else.
//!
//! Two inbound allow rules share one name: UDP for the Holochain endpoint and
//! TCP for the pairing service. Each is limited to this program, this local
//! address and port, peers on the local subnet, and wireless interfaces. The
//! rules are removed when [`InboundRules`] is dropped; if the process is
//! killed first, remove them with
//! `netsh advfirewall firewall delete rule name=<name>`.
//!
//! Adding rules needs an elevated process. Without one, Windows shows its own
//! "allow access" prompt the first time the program listens, which creates
//! broader per-program rules; the docs explain both.

use crate::{Result, WifiError};
use std::net::IpAddr;
use std::path::Path;

/// Prefix of every rule name this program creates.
pub const RULE_PREFIX: &str = "hc-wfd";

/// Scoped inbound rules, removed on drop.
#[derive(Debug)]
pub struct InboundRules {
    name: String,
    active: bool,
}

impl InboundRules {
    /// Allow UDP `udp_port` and TCP `tcp_port` to `local_ip`, for `program`.
    pub fn add(
        name: &str,
        program: &Path,
        local_ip: IpAddr,
        udp_port: u16,
        tcp_port: u16,
    ) -> Result<Self> {
        if !cfg!(windows) {
            return Err(WifiError::Unsupported(
                "Windows Firewall rules exist only on Windows",
            ));
        }
        let mut rules = Self {
            name: name.to_string(),
            active: true,
        };
        for (protocol, port) in [("UDP", udp_port), ("TCP", tcp_port)] {
            let args = add_rule_args(name, program, local_ip, protocol, port);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Err(err) = crate::run("netsh", &args, &format!("netsh {}", args.join(" "))) {
                // Leave nothing half-added behind.
                let _ = rules.remove_now();
                return Err(match err {
                    WifiError::Command { stderr, .. } if stderr.contains("elevation") => {
                        WifiError::Failed(
                            "adding firewall rules needs an elevated (Run as administrator) \
                             terminal"
                                .into(),
                        )
                    }
                    other => other,
                });
            }
        }
        Ok(rules)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Remove the rules now, rather than on drop.
    pub fn remove(mut self) -> Result<()> {
        self.remove_now()
    }

    fn remove_now(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.active) {
            return Ok(());
        }
        delete_rules_named(&self.name)
    }
}

impl Drop for InboundRules {
    fn drop(&mut self) {
        let _ = self.remove_now();
    }
}

/// Delete every rule with this name.
pub fn delete_rules_named(name: &str) -> Result<()> {
    let rule = format!("name={name}");
    let args = ["advfirewall", "firewall", "delete", "rule", rule.as_str()];
    crate::run("netsh", &args, &format!("netsh {}", args.join(" "))).map(|_| ())
}

fn add_rule_args(
    name: &str,
    program: &Path,
    local_ip: IpAddr,
    protocol: &str,
    port: u16,
) -> Vec<String> {
    vec![
        "advfirewall".into(),
        "firewall".into(),
        "add".into(),
        "rule".into(),
        format!("name={name}"),
        "dir=in".into(),
        "action=allow".into(),
        "enable=yes".into(),
        "profile=any".into(),
        "interfacetype=wireless".into(),
        format!("protocol={protocol}"),
        format!("localip={local_ip}"),
        format!("localport={port}"),
        "remoteip=localsubnet".into(),
        format!("program={}", program.display()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_scoped_to_program_address_port_subnet_and_wireless() {
        let args = add_rule_args(
            "hc-wfd-abc",
            Path::new(r"C:\hc-wifi-target\release\hc-wfd.exe"),
            "192.168.137.1".parse().unwrap(),
            "UDP",
            47001,
        );
        for expected in [
            "name=hc-wfd-abc",
            "dir=in",
            "action=allow",
            "protocol=UDP",
            "localip=192.168.137.1",
            "localport=47001",
            "remoteip=localsubnet",
            "interfacetype=wireless",
            r"program=C:\hc-wifi-target\release\hc-wfd.exe",
        ] {
            assert!(
                args.iter().any(|a| a == expected),
                "missing {expected}: {args:?}"
            );
        }
        assert!(
            !args
                .iter()
                .any(|a| a.contains("any") && a.starts_with("localport"))
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn adding_rules_off_windows_is_unsupported() {
        let result = InboundRules::add(
            "hc-wfd-test",
            Path::new("/bin/true"),
            "127.0.0.1".parse().unwrap(),
            1,
            2,
        );
        assert!(matches!(result, Err(WifiError::Unsupported(_))));
    }
}
