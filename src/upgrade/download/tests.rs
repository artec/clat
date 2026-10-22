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
    let folder = upgrade_temp("progress-extract");
    let archive = folder.join("clat-test.tar.gz");
    fs::write(
        &archive,
        include_bytes!("../../../tests/fixtures/upgrade-progress.tar.gz"),
    )
    .unwrap();
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
