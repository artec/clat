//! Deterministic, bounded `.clatpkg` container.
//!
//! The format is deliberately small enough to audit and implement in the
//! single CLAT binary: `CLATPKG1`, one big-endian JSON-header length, the
//! header, then the sorted file bodies. Every body has an independent size
//! and SHA-256 in the signed package container.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

const MAGIC: &[u8; 8] = b"CLATPKG1";
const SCHEMA_VERSION: u32 = 1;
const MAX_HEADER_BYTES: u64 = 1024 * 1024;
const MAX_FILES: usize = 4_096;
const MAX_DEPTH: usize = 32;
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const MAX_BUNDLE_BYTES: u64 = MAX_TOTAL_BYTES + MAX_HEADER_BYTES + 12;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundleHeader {
    schema_version: u32,
    files: Vec<BundleFile>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundleFile {
    path: String,
    bytes: u64,
    sha256: String,
    executable: bool,
}

struct PackFile {
    record: BundleFile,
    source: PathBuf,
}

pub(crate) fn pack_directory(source: &Path, output: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("inspect package root {}: {error}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("package root must be a regular non-symlink directory".into());
    }
    if output.exists() {
        return Err(format!(
            "bundle output already exists: {}",
            output.display()
        ));
    }
    let source = source
        .canonicalize()
        .map_err(|error| format!("canonicalize package root: {error}"))?;
    let mut files = Vec::new();
    collect_files(&source, &source, 0, &mut files)?;
    files.sort_by(|left, right| left.record.path.cmp(&right.record.path));
    if files.is_empty() {
        return Err("package root is empty".into());
    }
    let header = BundleHeader {
        schema_version: SCHEMA_VERSION,
        files: files
            .iter()
            .map(|file| BundleFile {
                path: file.record.path.clone(),
                bytes: file.record.bytes,
                sha256: file.record.sha256.clone(),
                executable: file.record.executable,
            })
            .collect(),
    };
    let header =
        serde_json::to_vec(&header).map_err(|error| format!("serialize bundle header: {error}"))?;
    if header.len() as u64 > MAX_HEADER_BYTES {
        return Err(format!("bundle header exceeds {MAX_HEADER_BYTES} bytes"));
    }
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temp = parent.join(format!(".clatpkg-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut target = private_create_new(&temp)?;
        target
            .write_all(MAGIC)
            .and_then(|()| target.write_all(&(header.len() as u32).to_be_bytes()))
            .and_then(|()| target.write_all(&header))
            .map_err(|error| format!("write bundle header: {error}"))?;
        let mut buffer = vec![0_u8; 64 * 1024];
        for planned in &files {
            let mut input = open_regular_no_follow(&planned.source)?;
            let mut remaining = planned.record.bytes;
            while remaining > 0 {
                let limit = usize::try_from(remaining.min(buffer.len() as u64))
                    .expect("buffer-sized limit fits usize");
                let read = input
                    .read(&mut buffer[..limit])
                    .map_err(|error| format!("read {}: {error}", planned.source.display()))?;
                if read == 0 {
                    return Err(format!(
                        "package file changed while packing: {}",
                        planned.source.display()
                    ));
                }
                target
                    .write_all(&buffer[..read])
                    .map_err(|error| format!("write bundle body: {error}"))?;
                remaining -= read as u64;
            }
            if input
                .read(&mut buffer[..1])
                .map_err(|error| format!("verify {}: {error}", planned.source.display()))?
                != 0
            {
                return Err(format!(
                    "package file changed while packing: {}",
                    planned.source.display()
                ));
            }
        }
        target
            .sync_all()
            .map_err(|error| format!("sync bundle: {error}"))?;
        fs::rename(&temp, output).map_err(|error| format!("publish bundle: {error}"))?;
        sync_dir(parent)?;
        Ok(())
    })();
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub(crate) fn unpack_bundle(bundle: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        return Err(format!(
            "bundle destination already exists: {}",
            destination.display()
        ));
    }
    let mut input = open_regular_no_follow(bundle)?;
    let bytes = input
        .metadata()
        .map_err(|error| format!("inspect bundle: {error}"))?
        .len();
    if bytes > MAX_BUNDLE_BYTES {
        return Err(format!(
            "bundle is {bytes} bytes; the cap is {MAX_BUNDLE_BYTES}"
        ));
    }
    let result = (|| {
        let mut magic = [0_u8; 8];
        input
            .read_exact(&mut magic)
            .map_err(|error| format!("read bundle magic: {error}"))?;
        if &magic != MAGIC {
            return Err("unsupported .clatpkg magic".into());
        }
        let mut length = [0_u8; 4];
        input
            .read_exact(&mut length)
            .map_err(|error| format!("read bundle header length: {error}"))?;
        let length = u32::from_be_bytes(length) as u64;
        if length == 0 || length > MAX_HEADER_BYTES {
            return Err(format!("invalid bundle header length {length}"));
        }
        let mut header = vec![0_u8; length as usize];
        input
            .read_exact(&mut header)
            .map_err(|error| format!("read bundle header: {error}"))?;
        let header: BundleHeader = serde_json::from_slice(&header)
            .map_err(|error| format!("parse bundle header: {error}"))?;
        validate_header(&header)?;
        create_private_dir(destination)?;
        let mut buffer = vec![0_u8; 64 * 1024];
        for record in &header.files {
            let relative = validated_relative(&record.path)?;
            let target = destination.join(&relative);
            if let Some(parent) = target.parent() {
                create_private_dir(parent)?;
            }
            let mut output = private_create_new(&target)?;
            let mut hasher = Sha256::new();
            let mut remaining = record.bytes;
            while remaining > 0 {
                let limit = usize::try_from(remaining.min(buffer.len() as u64))
                    .expect("buffer-sized limit fits usize");
                let read = input
                    .read(&mut buffer[..limit])
                    .map_err(|error| format!("read bundle body {}: {error}", record.path))?;
                if read == 0 {
                    return Err(format!("bundle is truncated in `{}`", record.path));
                }
                hasher.update(&buffer[..read]);
                output
                    .write_all(&buffer[..read])
                    .map_err(|error| format!("write extracted `{}`: {error}", record.path))?;
                remaining -= read as u64;
            }
            let digest = format!("{:x}", hasher.finalize());
            if digest != record.sha256 {
                return Err(format!("bundle file digest mismatch for `{}`", record.path));
            }
            output
                .sync_all()
                .map_err(|error| format!("sync extracted `{}`: {error}", record.path))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(
                    &target,
                    fs::Permissions::from_mode(if record.executable { 0o700 } else { 0o600 }),
                )
                .map_err(|error| format!("chmod extracted `{}`: {error}", record.path))?;
            }
        }
        if input
            .read(&mut buffer[..1])
            .map_err(|error| format!("check bundle trailing data: {error}"))?
            != 0
        {
            return Err("bundle contains undeclared trailing data".into());
        }
        sync_tree_dirs(destination)?;
        Ok(())
    })();
    if result.is_err() && destination.exists() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn collect_files(
    root: &Path,
    directory: &Path,
    depth: usize,
    files: &mut Vec<PackFile>,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("package tree exceeds depth cap {MAX_DEPTH}"));
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("read package directory {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read package directory entry: {error}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect package entry {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "package contains symbolic link: {}",
                path.display()
            ));
        }
        if metadata.is_dir() {
            collect_files(root, &path, depth + 1, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(format!("package contains special file: {}", path.display()));
        }
        if files.len() >= MAX_FILES {
            return Err(format!("package exceeds file cap {MAX_FILES}"));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(format!(
                "package file {} exceeds {MAX_FILE_BYTES} bytes",
                path.display()
            ));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "package path escaped root".to_owned())?;
        let relative = relative
            .to_str()
            .ok_or_else(|| "package paths must be UTF-8".to_owned())?
            .replace(std::path::MAIN_SEPARATOR, "/");
        validated_relative(&relative)?;
        let (sha256, opened_bytes) = hash_file(&path)?;
        if opened_bytes != metadata.len() {
            return Err(format!(
                "package file changed while scanning: {}",
                path.display()
            ));
        }
        let total = files.iter().map(|file| file.record.bytes).sum::<u64>() + opened_bytes;
        if total > MAX_TOTAL_BYTES {
            return Err(format!("package exceeds {MAX_TOTAL_BYTES} total bytes"));
        }
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt as _;
            metadata.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        files.push(PackFile {
            record: BundleFile {
                path: relative,
                bytes: opened_bytes,
                sha256,
                executable,
            },
            source: path,
        });
    }
    Ok(())
}

fn validate_header(header: &BundleHeader) -> Result<(), String> {
    if header.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "unsupported bundle schema {}; expected {SCHEMA_VERSION}",
            header.schema_version
        ));
    }
    if header.files.is_empty() || header.files.len() > MAX_FILES {
        return Err(format!("invalid bundle file count {}", header.files.len()));
    }
    let mut seen = BTreeSet::new();
    let mut previous: Option<&str> = None;
    let mut total = 0_u64;
    for file in &header.files {
        validated_relative(&file.path)?;
        if previous.is_some_and(|value| value >= file.path.as_str()) {
            return Err("bundle file table is not strictly sorted".into());
        }
        previous = Some(&file.path);
        if !seen.insert(&file.path) {
            return Err(format!("duplicate bundle path `{}`", file.path));
        }
        if file.bytes > MAX_FILE_BYTES {
            return Err(format!("bundle file `{}` exceeds size cap", file.path));
        }
        if file.sha256.len() != 64 || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("bundle file `{}` has invalid sha256", file.path));
        }
        total = total
            .checked_add(file.bytes)
            .ok_or_else(|| "bundle total size overflow".to_owned())?;
        if total > MAX_TOTAL_BYTES {
            return Err(format!("bundle exceeds {MAX_TOTAL_BYTES} total bytes"));
        }
    }
    Ok(())
}

fn validated_relative(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path.contains('\\') {
        return Err(format!("invalid bundle path `{path}`"));
    }
    let path = Path::new(path);
    let mut depth = 0_usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => depth += 1,
            _ => return Err(format!("invalid bundle path `{}`", path.display())),
        }
    }
    if depth == 0 || depth > MAX_DEPTH {
        return Err(format!(
            "invalid bundle path depth for `{}`",
            path.display()
        ));
    }
    Ok(path.to_path_buf())
}

fn hash_file(path: &Path) -> Result<(String, u64), String> {
    let mut file = open_regular_no_follow(path)?;
    let bytes = file
        .metadata()
        .map_err(|error| format!("inspect opened file {}: {error}", path.display()))?
        .len();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok((format!("{:x}", hasher.finalize()), bytes))
}

fn open_regular_no_follow(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("open regular file {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect opened file {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("path is not a regular file: {}", path.display()));
    }
    Ok(file)
}

fn private_create_new(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("create {}: {error}", path.display()))
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("create directory {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("chmod directory {}: {error}", path.display()))?;
    }
    Ok(())
}

fn sync_tree_dirs(root: &Path) -> Result<(), String> {
    let mut directories = vec![root.to_path_buf()];
    for entry in fs::read_dir(root).map_err(|error| format!("read extracted tree: {error}"))? {
        let path = entry
            .map_err(|error| format!("read extracted entry: {error}"))?
            .path();
        if path.is_dir() {
            collect_directories(&path, &mut directories)?;
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        sync_dir(&directory)?;
    }
    Ok(())
}

fn collect_directories(path: &Path, directories: &mut Vec<PathBuf>) -> Result<(), String> {
    directories.push(path.to_path_buf());
    for entry in fs::read_dir(path).map_err(|error| format!("read directory: {error}"))? {
        let path = entry
            .map_err(|error| format!("read directory entry: {error}"))?
            .path();
        if path.is_dir() {
            collect_directories(&path, directories)?;
        }
    }
    Ok(())
}

fn sync_dir(path: &Path) -> Result<(), String> {
    crate::private_fs::sync_dir(path)
        .map_err(|error| format!("sync directory {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("clat-bundle-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn round_trip_is_deterministic_and_preserves_bytes() {
        let root = temp_dir("roundtrip");
        let source = root.join("source");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("z.txt"), b"zeta").unwrap();
        fs::write(source.join("nested/a.txt"), b"alpha").unwrap();
        let first = root.join("first.clatpkg");
        let second = root.join("second.clatpkg");
        pack_directory(&source, &first).unwrap();
        pack_directory(&source, &second).unwrap();
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
        let destination = root.join("unpacked");
        unpack_bundle(&first, &destination).unwrap();
        assert_eq!(fs::read(destination.join("z.txt")).unwrap(), b"zeta");
        assert_eq!(
            fs::read(destination.join("nested/a.txt")).unwrap(),
            b"alpha"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_digest_tampering_and_removes_partial_tree() {
        let root = temp_dir("tamper");
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file"), b"original").unwrap();
        let bundle = root.join("package.clatpkg");
        pack_directory(&source, &bundle).unwrap();
        let mut bytes = fs::read(&bundle).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(&bundle, bytes).unwrap();
        let destination = root.join("unpacked");
        let error = unpack_bundle(&bundle, &destination).unwrap_err();
        assert!(error.contains("digest mismatch"), "{error}");
        assert!(!destination.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_trailing_bytes() {
        let root = temp_dir("trailing");
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file"), b"body").unwrap();
        let bundle = root.join("package.clatpkg");
        pack_directory(&source, &bundle).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&bundle)
            .unwrap()
            .write_all(b"x")
            .unwrap();
        let error = unpack_bundle(&bundle, &root.join("unpacked")).unwrap_err();
        assert!(error.contains("trailing data"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    fn write_raw_bundle(path: &Path, header: &BundleHeader, body: &[u8]) {
        let encoded = serde_json::to_vec(header).unwrap();
        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC).unwrap();
        file.write_all(&(encoded.len() as u32).to_be_bytes())
            .unwrap();
        file.write_all(&encoded).unwrap();
        file.write_all(body).unwrap();
    }

    #[test]
    fn rejects_escape_duplicate_and_truncated_file_tables() {
        let root = temp_dir("malicious-table");
        let escape = root.join("escape.clatpkg");
        write_raw_bundle(
            &escape,
            &BundleHeader {
                schema_version: 1,
                files: vec![BundleFile {
                    path: "../outside".into(),
                    bytes: 0,
                    sha256: format!("{:x}", Sha256::digest([])),
                    executable: false,
                }],
            },
            b"",
        );
        let error = unpack_bundle(&escape, &root.join("escape-output")).unwrap_err();
        assert!(error.contains("invalid bundle path"), "{error}");
        assert!(!root.join("outside").exists());

        let duplicate = root.join("duplicate.clatpkg");
        let record = BundleFile {
            path: "same".into(),
            bytes: 0,
            sha256: format!("{:x}", Sha256::digest([])),
            executable: false,
        };
        write_raw_bundle(
            &duplicate,
            &BundleHeader {
                schema_version: 1,
                files: vec![
                    BundleFile {
                        path: record.path.clone(),
                        bytes: record.bytes,
                        sha256: record.sha256.clone(),
                        executable: record.executable,
                    },
                    record,
                ],
            },
            b"",
        );
        let error = unpack_bundle(&duplicate, &root.join("duplicate-output")).unwrap_err();
        assert!(
            error.contains("not strictly sorted") || error.contains("duplicate"),
            "{error}"
        );

        let truncated = root.join("truncated.clatpkg");
        write_raw_bundle(
            &truncated,
            &BundleHeader {
                schema_version: 1,
                files: vec![BundleFile {
                    path: "file".into(),
                    bytes: 4,
                    sha256: format!("{:x}", Sha256::digest(b"body")),
                    executable: false,
                }],
            },
            b"bo",
        );
        let error = unpack_bundle(&truncated, &root.join("truncated-output")).unwrap_err();
        assert!(error.contains("truncated"), "{error}");
        assert!(!root.join("truncated-output").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
