use super::*;
pub(super) fn choices() -> Vec<String> {
    let mut result = Vec::new();
    #[cfg(unix)]
    {
        if let Ok(shell) = std::env::var("SHELL")
            && Path::new(&shell).is_absolute()
            && Path::new(&shell).is_file()
        {
            result.push(shell);
        }
        for shell in [
            "/bin/zsh",
            "/bin/bash",
            "/bin/sh",
            "/usr/bin/fish",
            "/usr/local/bin/fish",
            "/opt/homebrew/bin/fish",
        ] {
            if Path::new(shell).is_file() && !result.iter().any(|r| r == shell) {
                result.push(shell.into());
            }
        }
    }
    #[cfg(windows)]
    {
        for shell in ["pwsh.exe", "powershell.exe", "cmd.exe", "bash.exe"] {
            if let Some(path) = find(shell) {
                result.push(path);
            }
        }
    }
    result
}
#[cfg(windows)]
fn find(name: &str) -> Option<String> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}
pub(super) fn args(shell: &str) -> Vec<String> {
    let name = Path::new(shell)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if matches!(name.as_str(), "powershell" | "pwsh") {
        vec!["-NoLogo".into()]
    } else if name == "cmd" {
        Vec::new()
    } else {
        vec!["-l".into()]
    }
}
