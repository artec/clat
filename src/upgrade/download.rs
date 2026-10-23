use super::*;

type Reporter<'a> = &'a mut dyn FnMut(UpgradeProgress);

/// Fetch once, verify the signed manifest/hash, and only then extract.
pub(super) fn download_asset(
    asset: &ReleaseAsset,
    checksum: &ReleaseAsset,
    index: &ReleaseAsset,
    work_dir: &Path,
    release_tag: &str,
    report: Reporter<'_>,
) -> Result<PathBuf, String> {
    for candidate in [asset, checksum, index] {
        validate_asset_name(&candidate.name)?;
    }
    if !is_allowed_download_host(&asset.url) {
        return Err(format!(
            "download {}: host not in the GitHub allowlist ({})",
            asset.name, asset.url
        ));
    }
    report(UpgradeProgress::Downloading);
    let (slot, actual) = download_to_slot(asset, work_dir)?;
    prepare_downloaded_asset(asset, slot, work_dir, report, || {
        verify_checksum(checksum, index, &actual, &asset.name, release_tag)
    })
}

fn download_to_slot(asset: &ReleaseAsset, work_dir: &Path) -> Result<(PathBuf, String), String> {
    let response = agent()
        .get(&asset.url)
        .header("User-Agent", format!("clat/{}", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|error| format!("download {}: {error}", asset.name))?;
    if !response.status().is_success() {
        return Err(format!(
            "download {}: HTTP {}",
            asset.name,
            response.status()
        ));
    }
    const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;
    if response
        .headers()
        .get("Content-Length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_ASSET_BYTES)
    {
        return Err(format!(
            "download {}: asset larger than {MAX_ASSET_BYTES} bytes",
            asset.name
        ));
    }
    let slot = work_dir.join(if is_bare_binary(&asset.name) {
        "clat-download"
    } else {
        &asset.name
    });
    let mut file = fs::File::create(&slot)
        .map_err(|error| format!("stage download {}: {error}", asset.name))?;
    let (_, mut body) = response.into_parts();
    match stream_body_to_file(body.as_reader(), &mut file, MAX_ASSET_BYTES, &asset.name) {
        Ok(hash) => Ok((slot, hash)),
        Err(error) => {
            drop(file);
            let _ = fs::remove_file(&slot);
            Err(error)
        }
    }
}

fn prepare_downloaded_asset(
    asset: &ReleaseAsset,
    slot: PathBuf,
    work_dir: &Path,
    report: Reporter<'_>,
    verify: impl FnOnce() -> Result<(), String>,
) -> Result<PathBuf, String> {
    report(UpgradeProgress::Verifying);
    verify()?;
    let binary = if is_bare_binary(&asset.name) {
        verify_executable_file(&slot)?;
        slot
    } else {
        report(UpgradeProgress::Extracting);
        extract_asset(asset, &slot, work_dir)?
    };
    report(UpgradeProgress::Verified);
    Ok(binary)
}

fn extract_asset(asset: &ReleaseAsset, archive: &Path, work_dir: &Path) -> Result<PathBuf, String> {
    validate_archive(archive, &asset.name)?;
    // Windows ships bsdtar; Unix retains the existing unzip path for ZIPs.
    let output = if asset.name.ends_with(".zip") && !cfg!(windows) {
        Command::new("unzip")
            .arg("-o")
            .arg(archive)
            .arg("-d")
            .arg(work_dir)
            .output()
    } else {
        Command::new("tar")
            .arg(if asset.name.ends_with(".zip") {
                "-xf"
            } else {
                "-xzf"
            })
            .arg(archive)
            .arg("-C")
            .arg(work_dir)
            .output()
    }
    .map_err(|error| format!("launch extractor for {}: {error}", asset.name))?;
    if !output.status.success() {
        return Err(format!(
            "extract {}: {}",
            asset.name,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    reject_extracted_links(work_dir)?;
    let stem = asset.name.split('.').next().unwrap_or("clat");
    for candidate in ["clat.exe", "clat", stem] {
        let path = work_dir.join(candidate);
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            verify_executable_file(&path)?;
            return Ok(path);
        }
    }
    Err(format!(
        "extract {}: binary not found in archive",
        asset.name
    ))
}

#[cfg(test)]
mod tests;
