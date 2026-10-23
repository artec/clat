use super::*;

/// Ephemeral observations of CLI self-update; never journaled as run events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpgradeProgress {
    Checking { current: &'static str },
    Found { tag: String },
    Downloading,
    Verifying,
    Extracting,
    Verified,
    Installing,
    Installed { tag: String },
}

type Reporter<'a> = &'a mut dyn FnMut(UpgradeProgress);

/// Preserve the silent library entry point for existing callers.
pub fn upgrade(check_only: bool) -> Result<UpgradeOutcome, String> {
    upgrade_with_progress(check_only, |_| {})
}

/// Report actual phase boundaries without adding terminal I/O to core.
pub fn upgrade_with_progress(
    check_only: bool,
    mut report: impl FnMut(UpgradeProgress),
) -> Result<UpgradeOutcome, String> {
    check_and_upgrade(
        check_only,
        &mut report,
        || fetch_latest_release(std::env::var("GITHUB_TOKEN").ok().as_deref()),
        install_release,
    )
}

fn check_and_upgrade(
    check_only: bool,
    report: Reporter<'_>,
    fetch: impl FnOnce() -> Result<Release, String>,
    install: impl FnOnce(&Release, Reporter<'_>) -> Result<(), String>,
) -> Result<UpgradeOutcome, String> {
    let current = env!("CARGO_PKG_VERSION");
    report(UpgradeProgress::Checking { current });
    let release = fetch()?;
    if !is_newer(&release.tag, current) {
        return Ok(UpgradeOutcome::UpToDate {
            latest: release.tag,
        });
    }
    report(UpgradeProgress::Found {
        tag: release.tag.clone(),
    });
    if check_only {
        return Ok(UpgradeOutcome::Available { tag: release.tag });
    }
    install(&release, report)?;
    report(UpgradeProgress::Installed {
        tag: release.tag.clone(),
    });
    Ok(UpgradeOutcome::Installed { tag: release.tag })
}

fn required_assets(
    release: &Release,
) -> Result<(&ReleaseAsset, &ReleaseAsset, &ReleaseAsset), String> {
    let asset = select_asset(&release.assets).ok_or_else(|| {
        format!(
            "release {} has no {} binary asset",
            release.tag,
            target_triple()
        )
    })?;
    let checksum_name = format!("{}.sha256", asset.name);
    let find = |name: &str, kind: &str| {
        release
            .assets
            .iter()
            .find(|candidate| candidate.name == name)
            .ok_or_else(|| {
                format!(
                    "release {} is missing required {kind} asset {name}",
                    release.tag
                )
            })
    };
    for candidate in [
        asset,
        find(&checksum_name, "checksum")?,
        find("latest.json", "release index")?,
    ] {
        let expected = format!(
            "https://github.com/{REPO}/releases/download/{}/{}",
            release.tag, candidate.name
        );
        if candidate.url != expected {
            return Err("release asset URL does not match repository, tag and filename".into());
        }
    }
    Ok((
        asset,
        find(&checksum_name, "checksum")?,
        find("latest.json", "release index")?,
    ))
}

fn install_release(release: &Release, report: Reporter<'_>) -> Result<(), String> {
    // Missing signed assets fail before creating a directory or downloading.
    let (asset, checksum, index) = required_assets(release)?;
    let exe = std::env::current_exe().map_err(|error| format!("locate clat binary: {error}"))?;
    let work_dir = create_work_dir()?;
    let result =
        super::download::download_asset(asset, checksum, index, &work_dir, &release.tag, report)
            .and_then(|binary| install_binary(&binary, &exe, report));
    let _ = fs::remove_dir_all(&work_dir);
    result
}

fn install_binary(binary: &Path, exe: &Path, report: Reporter<'_>) -> Result<(), String> {
    report(UpgradeProgress::Installing);
    replace_binary(binary, exe)
}

fn create_work_dir() -> Result<PathBuf, String> {
    // Exclusive creation retains the existing no-precreated-link invariant.
    for attempt in 0..5 {
        let candidate = std::env::temp_dir().join(format!(
            "clat-upgrade-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(attempt as u128),
            attempt
        ));
        if fs::create_dir(&candidate).is_ok() {
            return Ok(candidate);
        }
    }
    Err("create work dir: all attempts failed".into())
}

#[cfg(test)]
mod tests;
