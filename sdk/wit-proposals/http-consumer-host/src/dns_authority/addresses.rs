use super::{Failure, nat64};
use std::net::IpAddr;
use std::sync::OnceLock;

struct Cidr {
    base: IpAddr,
    prefix: u32,
}
impl Cidr {
    fn contains(&self, ip: IpAddr) -> bool {
        match (self.base, ip) {
            (IpAddr::V4(base), IpAddr::V4(ip)) => {
                u32::from(base).checked_shr(32 - self.prefix)
                    == u32::from(ip).checked_shr(32 - self.prefix)
            }
            (IpAddr::V6(base), IpAddr::V6(ip)) => {
                u128::from(base).checked_shr(128 - self.prefix)
                    == u128::from(ip).checked_shr(128 - self.prefix)
            }
            _ => false,
        }
    }
}
fn special_ranges() -> &'static [Cidr] {
    static RANGES: OnceLock<Vec<Cidr>> = OnceLock::new();
    RANGES.get_or_init(|| {
        let data: serde_json::Value =
            serde_json::from_str(include_str!("data/special-ranges.json"))
                .expect("pinned IANA JSON");
        ["ipv4", "ipv6"]
            .into_iter()
            .flat_map(|family| data["ranges"][family].as_array().unwrap())
            .map(|range| {
                let (base, prefix) = range.as_str().unwrap().split_once('/').unwrap();
                Cidr {
                    base: base.parse().expect("pinned IANA address"),
                    prefix: prefix.parse().expect("pinned IANA prefix"),
                }
            })
            .collect()
    })
}
pub(super) fn is_public(ip: IpAddr) -> bool {
    let unicast = match ip {
        IpAddr::V4(ip) => !ip.is_multicast(),
        IpAddr::V6(ip) => ip.octets()[0] & 0xe0 == 0x20,
    };
    unicast && !special_ranges().iter().any(|cidr| cidr.contains(ip))
}
pub fn validate_addresses(
    addresses: Vec<IpAddr>,
    discovery: Vec<IpAddr>,
) -> Result<Vec<IpAddr>, Failure> {
    if addresses.len() > 32 {
        return Err(Failure::LimitExceeded);
    }
    if addresses.is_empty() {
        return Err(Failure::ResolutionFailed);
    }
    let prefixes = if addresses.iter().any(IpAddr::is_ipv6) {
        nat64::discover(&discovery)?
    } else {
        vec![]
    };
    for address in &addresses {
        if !is_public(*address) {
            return Err(Failure::BlockedAddress);
        }
        if let IpAddr::V6(ip) = address {
            nat64::validate(*ip, &prefixes)?;
        }
    }
    Ok(addresses)
}
