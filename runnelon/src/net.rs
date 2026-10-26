//! Minimal CIDR network type used to identify trusted proxy peers.
//!
//! Only what Runnelon needs: parse `addr/prefix` (or a bare address), mask host
//! bits, and test whether an [`IpAddr`] belongs to the network.

use std::net::IpAddr;
use std::str::FromStr;

/// An IPv4 or IPv6 network in CIDR notation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IpNet {
    network: IpAddr,
    prefix: u8,
}

impl IpNet {
    /// Build a network from an address and prefix length, masking host bits.
    pub fn new(addr: IpAddr, prefix: u8) -> Result<Self, String> {
        let max = match addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        if prefix > max {
            return Err(format!("prefix length {prefix} exceeds {max}"));
        }
        Ok(Self {
            network: mask(addr, prefix),
            prefix,
        })
    }

    /// Whether `addr` falls inside this network.
    pub fn contains(&self, addr: IpAddr) -> bool {
        match (self.network, addr) {
            (IpAddr::V4(network), IpAddr::V4(addr)) => {
                mask(IpAddr::V4(addr), self.prefix) == IpAddr::V4(network)
            }
            (IpAddr::V6(network), IpAddr::V6(addr)) => {
                mask(IpAddr::V6(addr), self.prefix) == IpAddr::V6(network)
            }
            _ => false,
        }
    }
}

impl FromStr for IpNet {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (addr, prefix) = match value.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (value, None),
        };
        let addr = addr
            .parse::<IpAddr>()
            .map_err(|_| format!("invalid IP address '{addr}'"))?;
        let prefix = match prefix {
            Some(prefix) => prefix
                .parse::<u8>()
                .map_err(|_| format!("invalid prefix length '{prefix}'"))?,
            None if addr.is_ipv4() => 32,
            None => 128,
        };
        Self::new(addr, prefix)
    }
}

fn mask(addr: IpAddr, prefix: u8) -> IpAddr {
    match addr {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let masked = if prefix == 0 {
                0
            } else {
                bits & (u32::MAX << (32 - prefix))
            };
            IpAddr::V4(masked.into())
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let masked = if prefix == 0 {
                0
            } else {
                bits & (u128::MAX << (128 - prefix))
            };
            IpAddr::V6(masked.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(value: &str) -> IpNet {
        value.parse().expect("valid network")
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("valid ip")
    }

    #[test]
    fn matches_ipv4_prefixes() {
        let trusted = net("10.0.0.0/8");
        assert!(trusted.contains(ip("10.1.2.3")));
        assert!(trusted.contains(ip("10.255.255.255")));
        assert!(!trusted.contains(ip("11.0.0.1")));
        assert!(!trusted.contains(ip("::ffff:10.1.2.3")));
    }

    #[test]
    fn bare_address_is_a_host_route() {
        let trusted = net("192.0.2.7");
        assert!(trusted.contains(ip("192.0.2.7")));
        assert!(!trusted.contains(ip("192.0.2.8")));
    }

    #[test]
    fn host_bits_are_masked() {
        let trusted = net("10.9.9.9/8");
        assert_eq!(trusted, net("10.0.0.0/8"));
        assert!(trusted.contains(ip("10.0.0.1")));
    }

    #[test]
    fn handles_zero_and_full_prefixes() {
        let any = net("0.0.0.0/0");
        assert!(any.contains(ip("203.0.113.5")));
        assert!(!any.contains(ip("2001:db8::1")));

        let any_v6 = net("::/0");
        assert!(any_v6.contains(ip("2001:db8::1")));
        assert!(!any_v6.contains(ip("192.0.2.1")));

        let host = net("192.0.2.1/32");
        assert!(host.contains(ip("192.0.2.1")));
        assert!(!host.contains(ip("192.0.2.2")));
    }

    #[test]
    fn matches_ipv6_prefixes() {
        let trusted = net("2001:db8::/32");
        assert!(trusted.contains(ip("2001:db8:1::5")));
        assert!(!trusted.contains(ip("2001:db9::1")));
    }

    #[test]
    fn rejects_invalid_input() {
        assert!(net_result("10.0.0.0/33").is_err());
        assert!(net_result("2001:db8::/129").is_err());
        assert!(net_result("not-an-ip/8").is_err());
        assert!(net_result("10.0.0.0/abc").is_err());
        assert!(net_result("").is_err());
    }

    fn net_result(value: &str) -> Result<IpNet, String> {
        value.parse()
    }
}
