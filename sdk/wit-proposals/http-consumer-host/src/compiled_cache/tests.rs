use super::*;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
thread_local! { static COMPILES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
pub(super) fn record_compile() {
    COMPILES.with(|count| count.set(count.get() + 1));
}
#[test]
fn restart_reuses_compiled_code_instead_of_recompiling() {
    let root = fixture();
    COMPILES.with(|count| count.set(0));
    component(&Engine::default(), &source(), &root).unwrap();
    component(&Engine::default(), &source(), &root).unwrap();
    assert_eq!(
        COMPILES.with(|count| count.get()),
        1,
        "a second engine activation must reuse authenticated compiler output"
    );
    fs::remove_dir_all(root.parent().unwrap()).unwrap();
}
static NEXT: AtomicU64 = AtomicU64::new(0);
fn fixture() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "clat-compile-cache-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let storage = root.join("storage");
    fs::create_dir_all(&storage).unwrap();
    storage
}
fn source() -> Vec<u8> {
    vec![0, 97, 115, 109, 13, 0, 1, 0]
}
#[test]
fn cache_hit_poison_and_foreign_content_have_safe_terminal_states() {
    let root = fixture();
    let engine = Engine::default();
    let source = source();
    let name = address(&engine, &source);
    let dir = files::open(&root).unwrap();
    let secret = files::key(&dir).unwrap();
    let key = hmac::Key::new(hmac::HMAC_SHA256, &secret);
    component(&engine, &source, &root).unwrap();
    assert!(
        read(&engine, &dir, &key, &name).is_some(),
        "first compile must publish usable authenticated code"
    );
    let good = files::read(&dir, &name, MAX_CACHE_BYTES).unwrap();
    let mut poison = good.clone();
    let last = poison.len() - 1;
    poison[last] ^= 1;
    files::publish(&dir, &name, &poison, false).unwrap();
    assert!(
        read(&engine, &dir, &key, &name).is_none(),
        "modified native code must never reach deserialize"
    );
    component(&engine, &source, &root).unwrap();
    assert!(
        read(&engine, &dir, &key, &name).is_some(),
        "poisoned code must be repaired by recompilation"
    );
    let other = address(&engine, b"different WASM bytes");
    files::publish(&dir, &other, &good, false).unwrap();
    assert!(
        read(&engine, &dir, &key, &other).is_none(),
        "valid output for other source is not valid here"
    );
    let different_key = hmac::Key::new(hmac::HMAC_SHA256, &[42; 32]);
    assert!(
        read(&engine, &dir, &different_key, &name).is_none(),
        "foreign host code must be rejected"
    );
    // Windows directory capabilities intentionally deny deletion while live.
    drop(dir);
    fs::remove_dir_all(root.parent().unwrap()).unwrap();
}
#[test]
fn invalid_cache_location_falls_back_without_creating_external_state() {
    let root = fixture();
    fs::write(cache_path(&root).unwrap(), b"user file").unwrap();
    component(&Engine::default(), &source(), &root).unwrap();
    assert_eq!(fs::read(cache_path(&root).unwrap()).unwrap(), b"user file");
    fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[test]
fn truncated_cache_is_recompiled_and_an_invalid_key_is_never_replaced() {
    let root = fixture();
    let engine = Engine::default();
    let source = source();
    component(&engine, &source, &root).unwrap();
    fs::write(
        cache_path(&root).unwrap().join(address(&engine, &source)),
        [0; 3],
    )
    .unwrap();
    component(&engine, &source, &root).unwrap();
    let dir = files::open(&root).unwrap();
    let key = hmac::Key::new(hmac::HMAC_SHA256, &files::key(&dir).unwrap());
    assert!(read(&engine, &dir, &key, &address(&engine, &source)).is_some());
    fs::write(cache_path(&root).unwrap().join("host-key"), [1; 4]).unwrap();
    component(&engine, &source, &root).unwrap();
    assert_eq!(
        fs::read(cache_path(&root).unwrap().join("host-key")).unwrap(),
        vec![1; 4]
    );
    drop(dir);
    fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[test]
fn engine_configuration_invalidates_cache_without_reusing_guest_state() {
    let root = fixture();
    let first = Engine::default();
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    let second = Engine::new(&config).unwrap();
    let source = source();
    assert_ne!(address(&first, &source), address(&second, &source));
    component(&first, &source, &root).unwrap();
    component(&second, &source, &root).unwrap();
    assert!(
        cache_path(&root)
            .unwrap()
            .join(address(&second, &source))
            .is_file()
    );
    fs::remove_dir_all(root.parent().unwrap()).unwrap();
}
#[test]
fn concurrent_first_publication_has_one_complete_secret() {
    let root = fixture();
    let results: Vec<_> = (0..8)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || files::key(&files::open(&root).unwrap()).unwrap())
        })
        .collect();
    let keys: Vec<_> = results
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(keys.iter().all(|key| key == &keys[0]));
    assert_eq!(fs::read_dir(cache_path(&root).unwrap()).unwrap().count(), 1);
    fs::remove_dir_all(root.parent().unwrap()).unwrap();
}
#[cfg(unix)]
#[test]
fn symlink_cache_or_key_never_writes_to_external_paths() {
    use std::os::unix::fs::symlink;
    let root = fixture();
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, cache_path(&root).unwrap()).unwrap();
    component(&Engine::default(), &source(), &root).unwrap();
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    fs::remove_file(cache_path(&root).unwrap()).unwrap();
    let dir = files::open(&root).unwrap();
    let marker = outside.join("key");
    fs::write(&marker, [8; 32]).unwrap();
    symlink(&marker, cache_path(&root).unwrap().join("host-key")).unwrap();
    assert!(files::key(&dir).is_err());
    component(&Engine::default(), &source(), &root).unwrap();
    assert_eq!(fs::read(&marker).unwrap(), vec![8; 32]);
    fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[test]
#[ignore = "repeated CPU measurement; explicitly run in clean machine state"]
fn perf_sha_samples() {
    use sha2::Digest as OldDigest;
    let bytes = vec![0xa5; 61 * 1024 * 1024];
    for sample in 0..5 {
        let start = std::time::Instant::now();
        let old = sha2::Sha256::digest(std::hint::black_box(&bytes));
        let before = start.elapsed().as_secs_f64();
        let start = std::time::Instant::now();
        let new = crate::hashing::Sha256::digest(std::hint::black_box(&bytes));
        let after = start.elapsed().as_secs_f64();
        assert_eq!(
            &old[..],
            &new[..],
            "hardware and fallback must have identical SHA-256 identity"
        );
        println!(
            "PERF1_SHA {{\"sample\":{},\"before_seconds\":{before:.6},\"after_seconds\":{after:.6}}}",
            sample + 1
        );
    }
}

#[test]
#[ignore = "actual component repeated compile/cache measurement; supply CLAT_PERF_COMPONENT"]
fn perf_component_samples() {
    let path =
        std::env::var_os("CLAT_PERF_COMPONENT").expect("explicit component fixture required");
    let source = fs::read(path).unwrap();
    for sample in 0..3 {
        let root = fixture();
        let mut config = wasmtime::Config::new();
        config
            .consume_fuel(true)
            .epoch_interruption(true)
            .cranelift_opt_level(wasmtime::OptLevel::None);
        let engine = Engine::new(&config).unwrap();
        let start = std::time::Instant::now();
        let uncached = component(&engine, &source, &root).unwrap();
        let before = start.elapsed().as_secs_f64();
        drop(uncached);
        let engine = Engine::new(&config).unwrap();
        let start = std::time::Instant::now();
        let cached = component(&engine, &source, &root).unwrap();
        let after = start.elapsed().as_secs_f64();
        println!(
            "PERF1_COMPONENT {{\"sample\":{},\"bytes\":{},\"before_seconds\":{before:.6},\"after_seconds\":{after:.6}}}",
            sample + 1,
            source.len()
        );
        drop(cached);
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
}

#[test]
fn network_runtime_constructor_reuses_code_across_fresh_lanes() {
    let root = fixture();
    COMPILES.with(|count| count.set(0));
    drop(crate::runtime::Runtime::new_cached(&source(), &root).unwrap());
    drop(crate::runtime::Runtime::new_cached(&source(), &root).unwrap());
    assert_eq!(COMPILES.with(|count| count.get()), 1);
    fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[cfg(windows)]
#[test]
fn a_cache_acl_open_to_everyone_is_never_used_for_native_code() {
    let root = fixture();
    COMPILES.with(|count| count.set(0));
    let engine = Engine::default();
    component(&engine, &source(), &root).unwrap();
    assert_eq!(COMPILES.with(|count| count.get()), 1);
    assert!(files::key(&files::open(&root).unwrap()).is_ok());
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, READ_CONTROL, WRITE_DAC,
    };
    let directory = fs::OpenOptions::new()
        .read(true)
        .access_mode(READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(cache_path(&root).unwrap())
        .unwrap();
    // SAFETY: owned temporary test directory handle. A null DACL intentionally
    // grants everyone access and must never be accepted for compiler secrets.
    let result = unsafe {
        SetSecurityInfo(
            directory.as_raw_handle(),
            SE_FILE_OBJECT,
            windows_sys::Win32::Security::DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(result, 0);
    drop(directory);
    assert!(files::open(&root).is_err());
    component(&engine, &source(), &root).unwrap();
    assert_eq!(
        COMPILES.with(|count| count.get()),
        2,
        "public ACL must force safe recompilation"
    );
    fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[test]
fn live_compiled_components_do_not_lock_cache_publication_or_cleanup() {
    let root = fixture();
    let engine = Engine::default();
    let source = source();
    let compiled = component(&engine, &source, &root).unwrap();
    let loaded = component(&engine, &source, &root).unwrap();
    let name = address(&engine, &source);
    {
        let dir = files::open(&root).unwrap();
        let original = files::read(&dir, &name, MAX_CACHE_BYTES).unwrap();
        files::publish(&dir, &name, &original, false)
            .expect("live components must not lock atomic replacement");
    }
    fs::remove_dir_all(root.parent().unwrap())
        .expect("production cache handles must close before the component returns");
    // Both compilation and deserialization own their code independently of
    // the cache files, so deleting the directory must not invalidate them.
    for component in [&compiled, &loaded] {
        let mut store = wasmtime::Store::new(&engine, ());
        let linker = wasmtime::component::Linker::new(&engine);
        linker
            .instantiate(&mut store, component)
            .expect("cached code must remain usable after deleting its backing cache");
    }
}
