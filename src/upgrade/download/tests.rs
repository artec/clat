use super::*;
use crate::upgrade::tests::upgrade_temp;

fn asset(name: &str) -> ReleaseAsset {
    ReleaseAsset {
        name: name.into(),
        url: "https://github.com/unused".into(),
    }
}

#[test]
fn upgrade_bad_signature_never_extracts_or_reports_verified() {
    let folder = upgrade_temp("progress-signature");
    let archive = folder.join("clat-test.tar.gz");
    fs::write(
        &archive,
        include_bytes!("../../../tests/fixtures/upgrade-progress.tar.gz"),
    )
    .unwrap();
    let mut phases = vec![];
    let result = prepare_downloaded_asset(
        &asset("clat-test.tar.gz"),
        archive,
        &folder,
        &mut |p| phases.push(p),
        || {
            verify_release_assets(
                "v9999.0.0",
                "clat-test.tar.gz.sha256",
                include_bytes!("../../../tests/fixtures/release-manifest.sha256"),
                b"invalid minisign signature",
                "unused",
                "clat-test.tar.gz",
            )
        },
    );
    assert!(result.is_err());
    assert_eq!(phases, [UpgradeProgress::Verifying]);
    assert!(!folder.join("clat").exists());
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn upgrade_archive_reports_extraction_before_verified_executable() {
    let fixture = include_bytes!("../../../tests/fixtures/upgrade-progress.tar.gz");
    assert!(
        fixture.starts_with(&[0x1f, 0x8b, 0x08]),
        "the .tar.gz fixture must contain gzip bytes; permissive tar implementations can hide a mislabeled plain tar"
    );
    let folder = upgrade_temp("progress-extract");
    let archive = folder.join("clat-test.tar.gz");
    fs::write(&archive, fixture).unwrap();
    let mut phases = vec![];
    let binary = prepare_downloaded_asset(
        &asset("clat-test.tar.gz"),
        archive,
        &folder,
        &mut |p| phases.push(p),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(
        fs::read(binary).unwrap(),
        [0x7f, b'E', b'L', b'F', 1, 2, 3, 4]
    );
    assert_eq!(
        phases,
        [
            UpgradeProgress::Verifying,
            UpgradeProgress::Extracting,
            UpgradeProgress::Verified
        ]
    );
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn upgrade_bare_binary_skips_extraction_but_checks_executable_magic() {
    let folder = upgrade_temp("progress-bare");
    let slot = folder.join("download");
    let mut phases = vec![];
    for bytes in [b"<html>oops".as_slice(), b"\x7fELFtest".as_slice()] {
        fs::write(&slot, bytes).unwrap();
        phases.clear();
        let result = prepare_downloaded_asset(
            &asset("clat-v9999.0.0-test"),
            slot.clone(),
            &folder,
            &mut |p| phases.push(p),
            || Ok(()),
        );
        if bytes.starts_with(b"<") {
            assert!(result.is_err());
            assert_eq!(phases, [UpgradeProgress::Verifying]);
        } else {
            result.unwrap();
            assert_eq!(
                phases,
                [UpgradeProgress::Verifying, UpgradeProgress::Verified]
            );
        }
    }
    fs::remove_dir_all(folder).unwrap();
}

/// 2026-11-23 审计 B-3 判别：兼容分支（Unix 的 ZIP）必须在**解压之前**
/// 拒绝符号链接条目，与 tar 分支一致——不能先解压、落地之后再扫描。
#[cfg(unix)]
#[test]
fn upgrade_zip_with_a_symlink_entry_is_rejected_before_extraction() {
    let dir = upgrade_temp("zip-links");
    let asset_name = "clat-x86_64-unknown-linux-gnu.zip";
    let links = dir.join("links.zip");
    fs::write(
        &links,
        zip_with_entries([
            ("clat", "/bin/true", 0o120_777),
            ("README", "ok", 0o100_644),
        ]),
    )
    .unwrap();
    let error = validate_archive(&links, asset_name)
        .expect_err("a ZIP with a symlink entry must be rejected before extraction");
    assert!(error.contains("link entry"), "{error}");

    // A ZIP of ordinary files keeps passing the pre-check.
    let plain = dir.join("plain.zip");
    fs::write(
        &plain,
        zip_with_entries([("clat", "#!/bin/sh\n", 0o100_755)]),
    )
    .unwrap();
    validate_archive(&plain, asset_name).expect("regular ZIP entries are accepted");
    let _ = fs::remove_dir_all(&dir);
}

/// A stored (uncompressed) ZIP. Entry types live in the central directory's
/// external attributes, which is where zipinfo reads them.
#[cfg(unix)]
fn zip_with_entries<const N: usize>(entries: [(&str, &str, u32); N]) -> Vec<u8> {
    // version 2.0, no flags, stored, 1980-01-01 00:00.
    const FIXED: [u8; 10] = [20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0];
    let mut local = Vec::new();
    let mut central = Vec::new();
    for (name, data, mode) in entries {
        let (offset, size, crc) = (
            local.len() as u32,
            data.len() as u32,
            crc32(data.as_bytes()),
        );
        local.extend_from_slice(b"PK\x03\x04");
        local.extend_from_slice(&FIXED);
        local.extend_from_slice(&crc.to_le_bytes());
        local.extend_from_slice(&size.to_le_bytes());
        local.extend_from_slice(&size.to_le_bytes());
        local.extend_from_slice(&(name.len() as u16).to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(name.as_bytes());
        local.extend_from_slice(data.as_bytes());
        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes());
        central.extend_from_slice(&FIXED);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&(mode << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let mut zip = local;
    let central_offset = zip.len() as u32;
    zip.extend_from_slice(&central);
    zip.extend_from_slice(b"PK\x05\x06");
    zip.extend_from_slice(&[0; 4]);
    zip.extend_from_slice(&(N as u16).to_le_bytes());
    zip.extend_from_slice(&(N as u16).to_le_bytes());
    zip.extend_from_slice(&(central.len() as u32).to_le_bytes());
    zip.extend_from_slice(&central_offset.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip
}

#[cfg(unix)]
fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & u32::wrapping_sub(0, crc & 1));
        }
    }
    !crc
}
