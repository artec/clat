use super::*;

fn fixture(tag: &str) -> (PathBuf, PathBuf, Manifest) {
    let (root, _) = crate::test_support::roots(tag);
    let resources = root.join("resources");
    fs::create_dir_all(&resources).unwrap();
    let bytes = b"MZ-fixture-executable";
    fs::write(resources.join(executable_name()), bytes).unwrap();
    let digest = format!("{:x}", Sha256::digest(bytes));
    let value = serde_json::json!({"version":super::super::build_identity::product_version(),
        "target":crate::upgrade::target_triple(),"sha256":digest,"size":bytes.len(),
        "official":super::super::build_identity::official_release()});
    fs::write(resources.join("backend.json"), value.to_string()).unwrap();
    (root, resources, serde_json::from_value(value).unwrap())
}

#[test]
fn cache_checks_bytes_and_keeps_running_slots() {
    let (root, resources, _) = fixture("desktop-cache");
    let cache = root.join("cache");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o711)).unwrap();
        fs::create_dir(&cache).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o777)).unwrap();
    }
    let first = PackagedBackend::prepare(&resources, &cache).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
            0o700,
            "an existing runtime cache must be private too"
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o711,
            "cache preparation must not change permissions of existing parent directories"
        );
    }
    assert_ne!(first.executable, resources.join(executable_name()));
    assert!(first.executable.exists());
    let reopened = PackagedBackend::prepare(&resources, &cache).unwrap();
    assert_eq!(first.executable, reopened.executable);
    drop(reopened);
    let lease = open_lease(first.executable.parent().unwrap()).unwrap();
    assert!(
        lease.try_lock().is_err(),
        "active runtime cannot be removed"
    );
    fs::write(resources.join(executable_name()), b"tampered").unwrap();
    assert!(PackagedBackend::prepare(&resources, &cache).is_err());
    drop(first);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn manifest_rejects_wrong_version_target_and_digest() {
    let (root, _, mut m) = fixture("desktop-manifest");
    assert!(validate_manifest(&m).is_ok());
    m.version = "0.0.0".into();
    assert!(validate_manifest(&m).is_err());
    m.version = super::super::build_identity::product_version().into();
    m.target = "different-platform".into();
    assert!(validate_manifest(&m).is_err());
    m.target = crate::upgrade::target_triple().into();
    m.sha256 = "../escape".into();
    assert!(validate_manifest(&m).is_err());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cleanup_retains_current_previous_and_leased_runtimes() {
    let (root, resources, _) = fixture("desktop-cache-prune");
    let cache = root.join("runtimes");
    fs::create_dir(&cache).unwrap();
    let old = (0..4)
        .map(|i| {
            let path = cache.join(format!("1.0.{i}-{}", "a".repeat(64)));
            fs::create_dir(&path).unwrap();
            fs::write(path.join(executable_name()), b"old-runtime").unwrap();
            drop(open_lease(&path).unwrap());
            path
        })
        .collect::<Vec<_>>();
    let busy = open_lease(&old[0]).unwrap();
    busy.lock_shared().unwrap();
    let current = PackagedBackend::prepare(&resources, &cache).unwrap();
    assert!(current.executable.exists());
    assert!(
        old[0].join(executable_name()).exists(),
        "running runtime retained"
    );
    assert!((1..=2).contains(&old.iter().filter(|p| p.exists()).count()));
    drop(busy);
    // A failed earlier cleanup may leave a newer directory without a runtime.
    // It must not displace the last real previous backend in the retention set.
    let orphan = cache.join(format!("0.0.0-{}", "b".repeat(64)));
    fs::create_dir(&orphan).unwrap();
    fs::write(
        orphan.join("unrecognized-file"),
        b"do not remove unknown files",
    )
    .unwrap();
    drop(open_lease(&orphan).unwrap());
    let reopened = PackagedBackend::prepare(&resources, &cache).unwrap();
    assert_eq!(
        old.iter().filter(|p| p.exists()).count(),
        1,
        "one previous slot retained"
    );
    assert!(orphan.join("unrecognized-file").exists());
    drop((current, reopened));
    let _ = fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn cache_rejects_windows_directory_junctions() {
    let (root, resources, _) = fixture("desktop-cache-junction");
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    let link = root.join("cache");
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("test junction creation exceeded five seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(status.success(), "test junction must be created");
    assert!(PackagedBackend::prepare(&resources, &link).is_err());
    fs::remove_dir(link).unwrap();
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn cache_and_resources_reject_symlinks() {
    let (root, resources, _) = fixture("desktop-cache-links");
    let target = root.join("outside");
    fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, root.join("cache")).unwrap();
    assert!(PackagedBackend::prepare(&resources, &root.join("cache")).is_err());
    let binary = resources.join(executable_name());
    fs::rename(&binary, target.join("binary")).unwrap();
    std::os::unix::fs::symlink(target.join("binary"), binary).unwrap();
    assert!(PackagedBackend::prepare(&resources, &root.join("other")).is_err());
    let _ = fs::remove_dir_all(root);
}
#[test]
fn startup_resume_rechecks_trust_before_open_and_never_grants_trust() {
    use super::resume_project_with;
    let path = std::path::Path::new("remembered-project");
    let error = resume_project_with::<u8>(path, |_| Ok(true), |_, _| panic!("untrusted start"))
        .unwrap_err();
    assert_eq!(error.message().key, "desktop.restore_needs_trust");
    for locale in [
        crate::i18n::Locale::Simplified,
        crate::i18n::Locale::Traditional,
    ] {
        assert_ne!(
            error.render(crate::i18n::LocaleContext::new(locale)),
            error.raw
        );
    }
    let error = resume_project_with::<u8>(
        path,
        |_| Err("directory missing".into()),
        |_, _| panic!("missing start"),
    )
    .unwrap_err();
    assert_eq!(error.raw, "directory missing");
    let client = resume_project_with(
        path,
        |_| Ok(false),
        |selected, trust| {
            assert_eq!(selected, path);
            assert!(!trust, "saved UI preferences must never authorize trust");
            Ok(42)
        },
    )
    .unwrap();
    assert_eq!(client, 42);
}
