//! Capability-relative cache publication and atomic first-key creation.
use cap_std::fs::{Dir, OpenOptions};
use ring::rand::{SecureRandom, SystemRandom};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const KEY: &str = "host-key";
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn open(storage: &Path) -> io::Result<Dir> {
    if std::fs::symlink_metadata(storage)?.file_type().is_symlink() {
        return Err(io::Error::other("cache storage is a symlink"));
    }
    let location = super::cache_path(storage)?;
    let name = location
        .file_name()
        .ok_or_else(|| io::Error::other("cache has no name"))?;
    let parent = Dir::open_ambient_dir(
        location
            .parent()
            .ok_or_else(|| io::Error::other("cache has no parent"))?,
        cap_std::ambient_authority(),
    )?;
    let storage_name = storage
        .file_name()
        .ok_or_else(|| io::Error::other("storage has no name"))?;
    let storage_dir = cap_primitives::fs::open_dir_nofollow(
        &parent.try_clone()?.into_std_file(),
        Path::new(storage_name),
    )
    .map(Dir::from_std_file)?;
    #[cfg(unix)]
    let mut builder = cap_std::fs::DirBuilder::new();
    #[cfg(not(unix))]
    let builder = cap_std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use cap_std::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match parent.create_dir_with(name, &builder) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    // Open the directory itself without following a replacement symlink.
    let dir = cap_primitives::fs::open_dir_nofollow(
        &parent.try_clone()?.into_std_file(),
        Path::new(name),
    )
    .map(Dir::from_std_file)?;
    #[cfg(not(unix))]
    let _ = &storage_dir;
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        let metadata = dir.dir_metadata()?;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != storage_dir.dir_metadata()?.uid() {
            return Err(io::Error::other(
                "cache directory must be private and host owned",
            ));
        }
    }
    #[cfg(windows)]
    super::windows::private(&dir)?;
    Ok(dir)
}

pub(super) fn read(dir: &Dir, name: &str, cap: u64) -> io::Result<Vec<u8>> {
    let metadata = dir.symlink_metadata(name)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > cap {
        return Err(io::Error::other("cache must be a bounded regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    use cap_fs_ext::OpenOptionsFollowExt;
    options.follow(cap_primitives::fs::FollowSymlinks::No);
    let file = dir.open_with(name, &options)?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.len() > cap {
        return Err(io::Error::other("invalid opened cache file"));
    }
    #[cfg(unix)]
    if name == KEY {
        use cap_std::fs::MetadataExt;
        if opened.mode() & 0o077 != 0 || opened.uid() != dir.dir_metadata()?.uid() {
            return Err(io::Error::other("cache key must be private"));
        }
    }
    #[cfg(windows)]
    if name == KEY {
        super::windows::private(&file)?;
    }
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap || bytes.len() as u64 != opened.len() {
        return Err(io::Error::other("cache changed or exceeded cap"));
    }
    Ok(bytes)
}

pub(super) fn key(dir: &Dir) -> io::Result<[u8; 32]> {
    match read(dir, KEY, 32) {
        Ok(bytes) => bytes
            .try_into()
            .map_err(|_| io::Error::other("invalid cache key")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut secret = [0; 32];
            SystemRandom::new()
                .fill(&mut secret)
                .map_err(|_| io::Error::other("random key unavailable"))?;
            publish(dir, KEY, &secret, true)?;
            read(dir, KEY, 32)?
                .try_into()
                .map_err(|_| io::Error::other("invalid published key"))
        }
        Err(error) => Err(error),
    }
}

pub(super) fn publish(dir: &Dir, name: &str, bytes: &[u8], first: bool) -> io::Result<()> {
    let unique = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut random = [0; 16];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| io::Error::other("random name unavailable"))?;
    let temp = format!(".tmp-{}-{unique}-{:x?}", std::process::id(), random);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = dir.open_with(&temp, &options)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if first {
            // A concurrent publisher cannot replace the winning secret.
            match dir.hard_link(&temp, dir, name) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            dir.remove_file(&temp)?;
        } else {
            dir.rename(&temp, dir, name)?;
        }
        #[cfg(unix)]
        dir.open(".")?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = dir.remove_file(&temp);
    }
    result
}
