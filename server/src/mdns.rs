//! Advertises the session listener as [`MDNS_SERVICE`], so a client finds
//! the server with no flags.

use std::collections::HashMap;
use std::net::SocketAddr;

use bounce_common::protocol::MDNS_SERVICE;
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};

/// Registers one instance named after this host. It is answered until the
/// returned daemon shuts down, which also sends the goodbye.
///
/// # Errors
///
/// When the daemon cannot start or the service is malformed.
pub fn advertise(session: SocketAddr) -> Result<(ServiceDaemon, String), mdns_sd::Error> {
    let service = service(session, &hostname())?;
    let fullname = service.get_fullname().to_owned();
    let daemon = ServiceDaemon::new()?;
    daemon.register(service)?;
    Ok((daemon, fullname))
}

fn service(session: SocketAddr, hostname: &str) -> Result<ServiceInfo, mdns_sd::Error> {
    let host = format!("{hostname}.local.");
    let ip = session.ip();
    let port = session.port();
    let txt = None::<HashMap<String, String>>;
    let (mut service, interfaces) = if ip.is_unspecified() {
        let service = ServiceInfo::new(MDNS_SERVICE, hostname, &host, (), port, txt)?;
        // A socket bound to 0.0.0.0 accepts no IPv6, so it must not
        // advertise IPv6 addresses.
        let family = if ip.is_ipv4() {
            IfKind::IPv4
        } else {
            IfKind::All
        };
        (service.enable_addr_auto(), family)
    } else {
        let service = ServiceInfo::new(MDNS_SERVICE, hostname, &host, ip, port, txt)?;
        (service, IfKind::Addr(ip))
    };
    service.set_interfaces(vec![interfaces]);
    Ok(service)
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "bounce".to_owned())
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;

    #[test]
    fn names_the_instance_after_the_host_on_the_session_port() {
        let any = service("0.0.0.0:7700".parse().unwrap(), "den").unwrap();
        assert_eq!(any.get_fullname(), "den._bounce._tcp.local.");
        assert_eq!(any.get_hostname(), "den.local.");
        assert_eq!(any.get_port(), 7700);
        assert!(any.is_addr_auto());

        let bound = IpAddr::from([192, 168, 1, 5]);
        let one = service(SocketAddr::new(bound, 7700), "den").unwrap();
        assert!(!one.is_addr_auto());
        assert_eq!(one.get_addresses().iter().collect::<Vec<_>>(), [&bound]);
    }
}
