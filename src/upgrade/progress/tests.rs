use super::*;
use crate::upgrade::tests::upgrade_temp;

fn newer_release() -> Release {
    Release {
        tag: "v9999.0.0".into(),
        assets: vec![],
    }
}

#[test]
fn upgrade_check_only_reports_discovery_without_installing() {
    let mut phases = vec![];
    let result = check_and_upgrade(
        true,
        &mut |p| phases.push(p),
        || Ok(newer_release()),
        |_, _| {
            panic!("check-only must not download or replace files");
        },
    )
    .unwrap();
    assert!(matches!(result, UpgradeOutcome::Available { .. }));
    assert_eq!(
        phases,
        [
            UpgradeProgress::Checking {
                current: env!("CARGO_PKG_VERSION")
            },
            UpgradeProgress::Found {
                tag: "v9999.0.0".into()
            },
        ]
    );
}

#[test]
fn upgrade_current_and_lookup_failure_never_report_installation() {
    let mut phases = vec![];
    let result = check_and_upgrade(
        false,
        &mut |p| phases.push(p),
        || {
            Ok(Release {
                tag: format!("v{}", env!("CARGO_PKG_VERSION")),
                assets: vec![],
            })
        },
        |_, _| panic!("already current"),
    )
    .unwrap();
    assert!(matches!(result, UpgradeOutcome::UpToDate { .. }));
    assert_eq!(phases.len(), 1);
    assert!(matches!(phases[0], UpgradeProgress::Checking { .. }));
    phases.clear();
    let result = check_and_upgrade(
        false,
        &mut |p| phases.push(p),
        || Err("offline".into()),
        |_, _| panic!("lookup failed"),
    );
    assert!(matches!(result, Err(error) if error == "offline"));
    assert_eq!(phases.len(), 1);
}

#[test]
fn upgrade_missing_signature_fails_before_downloading() {
    let name = format!("clat-v9999.0.0-{}", target_triple());
    for suffixes in [vec![""], vec!["", ".sha256"]] {
        let release = Release {
            tag: "v9999.0.0".into(),
            assets: suffixes
                .iter()
                .map(|suffix| ReleaseAsset {
                    name: format!("{name}{suffix}"),
                    url: "https://github.com/unused".into(),
                })
                .collect(),
        };
        let mut phases = vec![];
        let result = check_and_upgrade(
            false,
            &mut |p| phases.push(p),
            || Ok(release),
            install_release,
        );
        assert!(result.is_err());
        assert_eq!(
            phases.len(),
            2,
            "missing trust assets must not start downloading"
        );
    }
}

#[test]
fn upgrade_success_is_reported_only_after_real_binary_replacement() {
    let folder = upgrade_temp("progress-install");
    let source = folder.join("new");
    let destination = folder.join("installed");
    fs::write(&source, b"new-image").unwrap();
    fs::write(&destination, b"old-image").unwrap();
    let mut phases = vec![];
    let result = check_and_upgrade(
        false,
        &mut |phase| {
            if matches!(phase, UpgradeProgress::Installed { .. }) {
                assert_eq!(fs::read(&destination).unwrap(), b"new-image");
            }
            phases.push(phase);
        },
        || Ok(newer_release()),
        |_, report| install_binary(&source, &destination, report),
    )
    .unwrap();
    assert!(matches!(result, UpgradeOutcome::Installed { .. }));
    assert_eq!(phases[2], UpgradeProgress::Installing);
    assert!(matches!(phases[3], UpgradeProgress::Installed { .. }));
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn upgrade_failed_swap_never_reports_success_and_keeps_original() {
    let folder = upgrade_temp("progress-failure");
    let source = folder.join("missing-source");
    let destination = folder.join("installed");
    fs::write(&destination, b"old-image").unwrap();
    let mut phases = vec![];
    let result = check_and_upgrade(
        false,
        &mut |p| phases.push(p),
        || Ok(newer_release()),
        |_, report| install_binary(&source, &destination, report),
    );
    assert!(result.is_err());
    assert_eq!(phases.last(), Some(&UpgradeProgress::Installing));
    assert!(
        !phases
            .iter()
            .any(|p| matches!(p, UpgradeProgress::Installed { .. }))
    );
    assert_eq!(fs::read(&destination).unwrap(), b"old-image");
    fs::remove_dir_all(folder).unwrap();
}
