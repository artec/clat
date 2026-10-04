use super::*;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}
fn embedded(length: usize, v4: Ipv4Addr) -> IpAddr {
    let mut bytes = Ipv6Addr::new(0x2001, 0x4860, 0, 0, 0, 0, 0, 0).octets();
    // Independently expressed RFC6052 positions, not the implementation helper.
    let slots: &[usize] = match length {
        32 => &[4, 5, 6, 7],
        40 => &[5, 6, 7, 9],
        48 => &[6, 7, 9, 10],
        56 => &[7, 9, 10, 11],
        64 => &[9, 10, 11, 12],
        96 => &[12, 13, 14, 15],
        _ => panic!(),
    };
    for (slot, octet) in slots.iter().zip(v4.octets()) {
        bytes[*slot] = octet;
    }
    Ipv6Addr::from(bytes).into()
}

#[test]
fn mixed_answers_reject_the_complete_set() {
    assert_eq!(
        validate_addresses(vec![ip("8.8.8.8"), ip("169.254.169.254")], vec![]),
        Err(Failure::BlockedAddress)
    );
}
#[test]
fn empty_and_oversized_sets_do_not_create_pins() {
    assert!(validate_addresses(vec![], vec![]).is_err());
    assert_eq!(
        validate_addresses(vec![ip("8.8.8.8"); 33], vec![]),
        Err(Failure::LimitExceeded)
    );
}
#[test]
fn special_and_transition_addresses_cannot_be_pinned() {
    for address in [
        "127.0.0.1",
        "100.64.0.1",
        "192.0.2.1",
        "198.18.0.1",
        "224.0.0.1",
        "240.0.0.1",
        "::ffff:8.8.8.8",
        "::8.8.8.8",
        "2002:808:808::",
        "2001:db8::1",
        "fc00::1",
        "fe80::1",
    ] {
        assert_eq!(
            validate_addresses(vec![ip(address)], vec![ip("192.0.0.170")]),
            Err(Failure::BlockedAddress),
            "{address}"
        );
    }
}
#[test]
fn ipv6_requires_valid_fixed_discovery_metadata() {
    for discovery in [vec![], vec![ip("8.8.8.8")], vec![ip("2001:4860::1234")]] {
        assert_eq!(
            validate_addresses(vec![ip("2606:4700:4700::1111")], discovery),
            Err(Failure::InvalidDiscovery)
        );
    }
}
macro_rules! nat64_layout {
    ($name:ident,$length:expr) => {
        #[test]
        fn $name() {
            let discovery = vec![
                embedded($length, Ipv4Addr::new(192, 0, 0, 170)),
                embedded($length, Ipv4Addr::new(192, 0, 0, 171)),
            ];
            for blocked in [
                Ipv4Addr::new(127, 0, 0, 1),
                Ipv4Addr::new(10, 1, 2, 3),
                Ipv4Addr::new(169, 254, 169, 254),
            ] {
                assert_eq!(
                    validate_addresses(vec![embedded($length, blocked)], discovery.clone()),
                    Err(Failure::BlockedAddress)
                );
            }
            let public = embedded($length, Ipv4Addr::new(8, 8, 8, 8));
            assert_eq!(
                validate_addresses(vec![public], discovery),
                Ok(vec![public])
            );
        }
    };
}
nat64_layout!(nat64_32, 32);
nat64_layout!(nat64_40, 40);
nat64_layout!(nat64_48, 48);
nat64_layout!(nat64_56, 56);
nat64_layout!(nat64_64, 64);
nat64_layout!(nat64_96, 96);

#[test]
fn discovery_malformed_member_rejects_whole_metadata_set() {
    let valid = embedded(96, Ipv4Addr::new(192, 0, 0, 170));
    for extra in [ip("8.8.8.8"), ip("2001:4860::1234")] {
        assert_eq!(
            validate_addresses(vec![ip("2606:4700:4700::1111")], vec![valid, extra]),
            Err(Failure::InvalidDiscovery)
        );
    }
}

#[test]
fn every_matching_nat64_prefix_is_checked() {
    // First /96 matches a public IPv4. A later overlapping /32 must still reject.
    let IpAddr::V6(base) = embedded(32, Ipv4Addr::new(10, 1, 2, 3)) else {
        panic!()
    };
    let mut bytes = base.octets();
    bytes[12..].copy_from_slice(&[8, 8, 8, 8]);
    let candidate = Ipv6Addr::from(bytes).into();
    bytes[12..].copy_from_slice(&[192, 0, 0, 170]);
    let discoveries = vec![
        Ipv6Addr::from(bytes).into(),
        embedded(32, Ipv4Addr::new(192, 0, 0, 170)),
    ];
    assert_eq!(
        validate_addresses(vec![candidate], discoveries),
        Err(Failure::BlockedAddress)
    );
}

#[test]
fn nat64_nonzero_reserved_octet_and_suffix_are_rejected() {
    for length in [32, 40, 48, 56, 64] {
        let discovery = vec![embedded(length, Ipv4Addr::new(192, 0, 0, 170))];
        let IpAddr::V6(public) = embedded(length, Ipv4Addr::new(8, 8, 8, 8)) else {
            panic!()
        };
        for slot in [8, 15] {
            let mut bytes = public.octets();
            bytes[slot] = 1;
            assert_eq!(
                validate_addresses(vec![Ipv6Addr::from(bytes).into()], discovery.clone()),
                Err(Failure::BlockedAddress)
            );
        }
    }
}
