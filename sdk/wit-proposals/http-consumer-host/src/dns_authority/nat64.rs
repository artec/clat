use super::{Failure, addresses::is_public};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const LENGTHS: [usize; 6] = [32, 40, 48, 56, 64, 96];
pub(super) struct Prefix {
    bytes: [u8; 16],
    length: usize,
}
fn extract(bytes: [u8; 16], length: usize) -> Option<Ipv4Addr> {
    if length == 96 {
        return Some(Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]));
    }
    if bytes[8] != 0 {
        return None;
    }
    let before = 8 - length / 8;
    let after = 4 - before;
    if bytes[9 + after..].iter().any(|b| *b != 0) {
        return None;
    }
    let mut v4 = [0; 4];
    v4[..before].copy_from_slice(&bytes[length / 8..8]);
    v4[before..].copy_from_slice(&bytes[9..9 + after]);
    Some(v4.into())
}
fn sentinel(ip: Ipv4Addr) -> bool {
    matches!(ip.octets(), [192, 0, 0, 170 | 171])
}
pub(super) fn discover(answers: &[IpAddr]) -> Result<Vec<Prefix>, Failure> {
    if answers.is_empty() || answers.len() > 32 {
        return Err(Failure::InvalidDiscovery);
    }
    let mut prefixes = Vec::new();
    for address in answers {
        match address {
            IpAddr::V4(ip) if sentinel(*ip) => {}
            IpAddr::V6(ip) => {
                let mut found = false;
                for length in LENGTHS {
                    if extract(ip.octets(), length).is_some_and(sentinel) {
                        let mut bytes = ip.octets();
                        bytes[length / 8..].fill(0);
                        prefixes.push(Prefix { bytes, length });
                        found = true;
                    }
                }
                if !found {
                    return Err(Failure::InvalidDiscovery);
                }
            }
            _ => return Err(Failure::InvalidDiscovery),
        }
    }
    Ok(prefixes)
}
pub(super) fn validate(ip: Ipv6Addr, prefixes: &[Prefix]) -> Result<(), Failure> {
    let bytes = ip.octets();
    for prefix in prefixes {
        if bytes[..prefix.length / 8] == prefix.bytes[..prefix.length / 8] {
            let embedded = extract(bytes, prefix.length).ok_or(Failure::BlockedAddress)?;
            if !is_public(embedded.into()) {
                return Err(Failure::BlockedAddress);
            }
        }
    }
    Ok(())
}
