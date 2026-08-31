//! JSON 控制面文件的统一读写纪律（MP-1 §4.3/§4.6）：读经 cap-std
//! capability 句柄、unit 版本门 fail-closed、撕裂进抢救路径；写走
//! tmp+rename+fsync（镜像 project.rs 的 W-INV1 原子纪律），0600。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

use super::timestamp;

/// `unit` 头：每个控制面 JSON 文件的身份与版本（DSH 容器形态）。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct UnitTag {
    pub name: String,
    pub version: u64,
}

impl UnitTag {
    pub(crate) fn new(name: &str, version: u64) -> Self {
        Self {
            name: name.to_owned(),
            version,
        }
    }

    pub(crate) fn matches(&self, expected: (&str, u64)) -> bool {
        self.name == expected.0 && self.version == expected.1
    }
}

/// 一次受纪律的读取结果。
pub(crate) enum Loaded<T> {
    /// 文件不存在：调用方取默认值（空设置是合法状态）。
    Missing,
    /// 完整加载。
    Intact(T),
    /// JSON 本身解析失败（撕裂/半写）：残件已改名保留，调用方用默认
    /// 值继续，诊断由调用方响亮上报（绝不静默重建——INV-MP3）。
    Salvaged { remnant: String },
}

/// 读取失败中**不可抢救**的形态（区别于撕裂）。
pub(crate) enum LoadError {
    /// `unit` 与本构建不符：fail-closed，绝不自动降级猜读（INV-MP6）。
    VersionMismatch {
        file: String,
        found: String,
    },
    /// JSON 合法但结构不是本文件族：异构状态，fail-closed。
    Malformed {
        file: String,
        reason: String,
    },
    Io(String),
}

impl LoadError {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::VersionMismatch { file, found } => format!(
                "{file} carries {found}; this build expects a different control-plane \
                 unit version and will not guess-read it (no-migration policy) — \
                 remove or rename the file manually and restart"
            ),
            Self::Malformed { file, reason } => format!(
                "{file} is valid JSON but not a CLAT {file}: {reason} — remove or \
                 rename the file manually and restart"
            ),
            Self::Io(reason) => format!("cannot read control-plane file: {reason}"),
        }
    }
}

/// 经 capability 句柄读取一个 unit 包装的 JSON 文件。
///
/// - 不存在 → `Missing`；
/// - JSON 解析失败（撕裂）→ 残件改名 `<name>.torn-<日期>` 保留，返回
///   `Salvaged`；
/// - JSON 合法但 `unit` 不匹配 / 结构异构 → `LoadError`（fail-closed）。
pub(crate) fn load<T: DeserializeOwned>(
    dir: &cap_std::fs::Dir,
    parent: &Path,
    name: &str,
    expected_unit: (&str, u64),
) -> Result<Loaded<T>, LoadError> {
    load_inner(dir, parent, name, expected_unit, None)
}

/// The same unit/version/torn-remnant discipline as [`load`], with a true
/// cap+1 read bound for stores whose valid maximum size is known.
pub(crate) fn load_limited<T: DeserializeOwned>(
    dir: &cap_std::fs::Dir,
    parent: &Path,
    name: &str,
    expected_unit: (&str, u64),
    max_bytes: usize,
) -> Result<Loaded<T>, LoadError> {
    load_inner(dir, parent, name, expected_unit, Some(max_bytes))
}

fn load_inner<T: DeserializeOwned>(
    dir: &cap_std::fs::Dir,
    parent: &Path,
    name: &str,
    expected_unit: (&str, u64),
    max_bytes: Option<usize>,
) -> Result<Loaded<T>, LoadError> {
    if dir
        .symlink_metadata(name)
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(LoadError::Io(format!("{name} must not be a symbolic link")));
    }
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = match dir.open_with(name, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded::Missing);
        }
        Err(error) => return Err(LoadError::Io(format!("cannot open {name}: {error}"))),
    };
    let metadata = file
        .metadata()
        .map_err(|error| LoadError::Io(format!("cannot inspect {name}: {error}")))?;
    if !metadata.is_file() {
        return Err(LoadError::Io(format!("{name} must be a regular file")));
    }
    if let Some(max_bytes) = max_bytes
        && metadata.len() > max_bytes as u64
    {
        return Err(LoadError::Io(format!(
            "{name} exceeds the {max_bytes}-byte read limit"
        )));
    }
    let mut bytes = Vec::new();
    match max_bytes {
        Some(max_bytes) => {
            std::io::Read::by_ref(&mut file)
                .take(max_bytes.saturating_add(1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|error| LoadError::Io(format!("cannot read {name}: {error}")))?;
            if bytes.len() > max_bytes {
                return Err(LoadError::Io(format!(
                    "{name} exceeds the {max_bytes}-byte read limit"
                )));
            }
        }
        None => {
            file.read_to_end(&mut bytes)
                .map_err(|error| LoadError::Io(format!("cannot read {name}: {error}")))?;
        }
    }
    // Windows does not permit the torn-remnant rename while this read handle
    // is still open. Close it before parsing can enter the salvage path.
    drop(file);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // 先按无类型 JSON 解析：失败 = 撕裂（抢救路径）；成功后再过 unit
    // 门与结构门（fail-closed）。
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => {
            let remnant = salvage_torn(dir, parent, name)?;
            return Ok(Loaded::Salvaged { remnant });
        }
    };
    // unit 头先验：版本错位是异常而非崩溃残迹——fail-closed 拒载，
    // 绝不自动降级猜读（INV-MP6）。
    if let Ok(shallow) = serde_json::from_value::<UnitTagShallow>(value.clone())
        && !shallow.unit.matches(expected_unit)
    {
        return Err(LoadError::VersionMismatch {
            file: name.to_owned(),
            found: format!("unit {} v{}", shallow.unit.name, shallow.unit.version),
        });
    }
    let parsed: T = serde_json::from_value(value).map_err(|error| LoadError::Malformed {
        file: name.to_owned(),
        reason: error.to_string(),
    })?;
    Ok(Loaded::Intact(parsed))
}

/// 只关心 `unit` 头的浅解析（结构门的前置探针）。
#[derive(Deserialize)]
struct UnitTagShallow {
    unit: UnitTag,
}

/// 原子写一个 JSON 文件：tmp（create_new + 0600）→ fsync → rename →
/// fsync 父目录。`parent` 是该文件的绝对父目录（用于目录 fsync——
/// Windows 上为 no-op，见 private_fs::sync_dir）。
pub(crate) fn write(
    dir: &cap_std::fs::Dir,
    parent: &Path,
    name: &str,
    value: &impl Serialize,
) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("cannot serialize {name}: {error}"))?;
    crate::private_fs::write_text_atomic(dir, parent, name, &text)
}

/// 撕裂残件改名保留（`<name>.torn-<日期>`，重名加序号）。返回新名。
fn salvage_torn(dir: &cap_std::fs::Dir, parent: &Path, name: &str) -> Result<String, LoadError> {
    let stamp = timestamp::date_stamp();
    let mut candidate = format!("{name}.torn-{stamp}");
    let mut counter = 2;
    loop {
        match dir.symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(LoadError::Io(format!(
                    "cannot inspect {candidate}: {error}"
                )));
            }
            Ok(_) => {
                candidate = format!("{name}.torn-{stamp}-{counter}");
                counter += 1;
            }
        }
    }
    dir.rename(name, dir, &candidate)
        .map_err(|error| LoadError::Io(format!("cannot preserve the torn {name}: {error}")))?;
    crate::private_fs::sync_dir(parent).ok();
    Ok(candidate)
}
