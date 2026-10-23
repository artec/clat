# CLAT installer for Windows (PowerShell).
#
# Usage:
#   irm https://raw.githubusercontent.com/artec/clat/main/install.ps1 | iex
#
# Detects the architecture, prefers a prebuilt binary from GitHub
# Releases, and falls back to building from source when no release exists
# yet (offering to install the Rust toolchain if cargo is missing).
param([string]$Lang = "")
$ErrorActionPreference = "Stop"

$Repo = "artec/clat"
$Bin = "clat"

function Info($m) { Write-Host "> $m" -ForegroundColor Cyan }
function Warn($m) { Write-Host "! $m" -ForegroundColor Yellow }
function Fail($m) { Write-Host "x $m" -ForegroundColor Red; exit 1 }

# BEGIN GENERATED LOCALES
# Generated from locales/*.json by bun scripts/i18n-installers.ts. Do not edit.
$script:InstallTexts = @{
'en' = @{
  'installer.arch' = 'unsupported {platform} architecture: {arch}'
  'installer.archive_binary' = 'release archive contains links or did not contain a regular {bin} file'
  'installer.archive_inspect' = 'cannot inspect release archive {asset}'
  'installer.archive_link_entry' = 'release archive contains a link entry'
  'installer.archive_links' = 'cannot inspect release archive links for {asset}'
  'installer.archive_path' = 'unsafe path in release archive: {entry}'
  'installer.build' = 'building CLAT from source (takes a few minutes)'
  'installer.checksum_invalid' = 'invalid checksum file for {asset} — aborting'
  'installer.checksum_mismatch' = 'checksum mismatch for {asset} — aborting'
  'installer.checksum_missing' = 'required checksum unavailable for {asset} — aborting'
  'installer.checksum_verify' = 'verifying checksum'
  'installer.done' = 'done'
  'installer.download' = 'downloading {url}'
  'installer.installed' = 'installed {bin} to {dest}'
  'installer.installing' = 'installing CLAT (github.com/{repo})'
  'installer.language_invalid' = 'unsupported installer language: {value}; use system|en|zh-Hans|zh-Hant'
  'installer.os' = 'unsupported OS: {os} — use install.ps1 on Windows'
  'installer.path' = 'add {dest} to your PATH, for example:  {command}'
  'installer.path_simple' = 'add {dest} to your PATH'
  'installer.rust' = 'Rust is required to build from source; installing via {installer}'
  'installer.source_fallback' = 'no prebuilt release for {target} — building from source'
  'installer.target' = 'detected target: {target}'
  'installer.usage' = 'usage: install.sh [--lang system|en|zh-Hans|zh-Hant]'
}
'zh-Hans' = @{
  'installer.arch' = '不支持的 {platform} 架构：{arch}'
  'installer.archive_binary' = '发布归档包含链接，或没有常规 {bin} 文件'
  'installer.archive_inspect' = '无法检查发布归档 {asset}'
  'installer.archive_link_entry' = '发布归档中含有链接项'
  'installer.archive_links' = '无法检查 {asset} 归档中的链接'
  'installer.archive_path' = '发布归档中包含不安全路径：{entry}'
  'installer.build' = '正在从源码构建 CLAT（需要几分钟）'
  'installer.checksum_invalid' = '{asset} 校验文件无效，已中止'
  'installer.checksum_mismatch' = '{asset} 校验和不匹配，已中止'
  'installer.checksum_missing' = '找不到 {asset} 所需校验和，已中止'
  'installer.checksum_verify' = '正在验证校验和'
  'installer.done' = '完成'
  'installer.download' = '正在下载 {url}'
  'installer.installed' = '已将 {bin} 安装到 {dest}'
  'installer.installing' = '正在安装 CLAT（github.com/{repo}）'
  'installer.language_invalid' = '不支持的安装语言：{value}；可选 system|en|zh-Hans|zh-Hant'
  'installer.os' = '不支持的系统：{os}，Windows 请使用 install.ps1'
  'installer.path' = '请将 {dest} 添加到 PATH，例如：{command}'
  'installer.path_simple' = '请将 {dest} 添加到 PATH'
  'installer.rust' = '源码构建需要 Rust，正在通过 {installer} 安装'
  'installer.source_fallback' = '没有 {target} 预编译版本，将从源码构建'
  'installer.target' = '检测到目标平台：{target}'
  'installer.usage' = '用法：install.sh [--lang system|en|zh-Hans|zh-Hant]'
}
'zh-Hant' = @{
  'installer.arch' = '不支援的 {platform} 架構：{arch}'
  'installer.archive_binary' = '發佈封存檔包含連結，或沒有一般 {bin} 檔案'
  'installer.archive_inspect' = '無法檢查發佈封存檔 {asset}'
  'installer.archive_link_entry' = '發佈封存檔中含有連結項'
  'installer.archive_links' = '無法檢查 {asset} 封存檔中的連結'
  'installer.archive_path' = '發佈封存檔中包含不安全路徑：{entry}'
  'installer.build' = '正在從原始碼建置 CLAT（需要幾分鐘）'
  'installer.checksum_invalid' = '{asset} 校驗檔案無效，已中止'
  'installer.checksum_mismatch' = '{asset} 校驗碼不符，已中止'
  'installer.checksum_missing' = '找不到 {asset} 所需校驗碼，已中止'
  'installer.checksum_verify' = '正在驗證校驗碼'
  'installer.done' = '完成'
  'installer.download' = '正在下載 {url}'
  'installer.installed' = '已將 {bin} 安裝至 {dest}'
  'installer.installing' = '正在安裝 CLAT（github.com/{repo}）'
  'installer.language_invalid' = '不支援的安裝語言：{value}；可選 system|en|zh-Hans|zh-Hant'
  'installer.os' = '不支援的系統：{os}，Windows 請使用 install.ps1'
  'installer.path' = '請將 {dest} 加入 PATH，例如：{command}'
  'installer.path_simple' = '請將 {dest} 加入 PATH'
  'installer.rust' = '原始碼建置需要 Rust，正在透過 {installer} 安裝'
  'installer.source_fallback' = '沒有 {target} 預先編譯版本，將從原始碼建置'
  'installer.target' = '偵測到目標平台：{target}'
  'installer.usage' = '用法：install.sh [--lang system|en|zh-Hans|zh-Hant]'
}
}
function Text($key, $parameters = @{}) {
    $value = $script:InstallTexts[$script:InstallLocale][$key]
    if ($null -eq $value) { $value = $script:InstallTexts['en'][$key] }
    return [regex]::Replace($value, '\{([A-Za-z][A-Za-z0-9_]*)\}', { param($match)
        $name = $match.Groups[1].Value
        if ($parameters.ContainsKey($name)) { [string]$parameters[$name] } else { $match.Value }
    })
}
# END GENERATED LOCALES

$preference = if ($Lang) { $Lang } elseif ($env:CLAT_LANG) { $env:CLAT_LANG } else { "system" }
$detected = if ($preference -eq "system") { [Globalization.CultureInfo]::CurrentUICulture.Name } else { $preference }
$normalized = $detected.ToLowerInvariant().Replace('_','-')
$script:InstallLocale = switch -Regex ($normalized) {
    'zh.*hant' { 'zh-Hant'; break }
    'zh.*hans' { 'zh-Hans'; break }
    '^zh-(tw|hk|mo)' { 'zh-Hant'; break }
    '^zh' { 'zh-Hans'; break }
    default { 'en' }
}
if ($preference -notmatch '^(system|en(-[a-zA-Z]+)?|zh(-Hans|-Hant|-CN|-SG|-TW|-HK|-MO)?)$') {
    $problem = Text "installer.language_invalid" @{value=$preference}
    if ($Lang) { Fail $problem } else { Warn $problem }
}

function Get-Target {
    switch ($env:PROCESSOR_ARCHITECTURE) {
        "ARM64" { return "aarch64-pc-windows-msvc" }
        "AMD64" { return "x86_64-pc-windows-msvc" }
        default { Fail (Text "installer.arch" @{platform="Windows";arch=$env:PROCESSOR_ARCHITECTURE}) }
    }
}

function Install-Binary($file) {
    $dest = Join-Path $env:LOCALAPPDATA "clat\bin"
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Copy-Item $file (Join-Path $dest "$Bin.exe") -Force
    Info (Text "installer.installed" @{bin="$Bin.exe";dest=$dest})
    if (-not (($env:Path -split ';') -contains $dest)) {
        Warn (Text "installer.path" @{dest=$dest;command="setx PATH `"$($env:Path);$dest`""})
    }
}

# Returns $true and installs the prebuilt binary, or $false when no
# release asset exists for this target.
function Install-FromRelease($target) {
    try {
        $release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    } catch {
        return $false
    }
    $tag = $release.tag_name
    if (-not $tag) { return $false }

    $asset = "clat-$tag-$target.zip"
    $url = "https://github.com/$Repo/releases/download/$tag/$asset"
    $tmp = Join-Path $env:TEMP ("clat-install-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        Info (Text "installer.download" @{url=$url})
        $archive = Join-Path $tmp $asset
        Invoke-WebRequest -Uri $url -OutFile $archive
        # The checksum is mandatory. Missing, malformed, misnamed, and
        # mismatched checksum files all abort the release install.
        $checksumFile = Join-Path $tmp "$asset.sha256"
        try {
            Invoke-WebRequest -Uri "$url.sha256" -OutFile $checksumFile
        } catch {
            Fail (Text "installer.checksum_missing" @{asset=$asset})
        }
        Info (Text "installer.checksum_verify")
        $checksumLines = @(Get-Content $checksumFile |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if ($checksumLines.Count -ne 1) {
            Fail (Text "installer.checksum_invalid" @{asset=$asset})
        }
        $checksumLine = $checksumLines[0]
        $checksumParts = @($checksumLine.Trim() -split '\s+')
        if ($checksumParts.Count -ne 2 -or
            $checksumParts[0] -notmatch '^[0-9a-fA-F]{64}$' -or
            $checksumParts[1].TrimStart('*') -ne $asset) {
            Fail (Text "installer.checksum_invalid" @{asset=$asset})
        }
        $expected = $checksumParts[0].ToLowerInvariant()
        $actual = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($expected -ne $actual) {
            Fail (Text "installer.checksum_mismatch" @{asset=$asset})
        }

        # Inspect every ZIP entry before extraction. Expand-Archive must never
        # see parent traversal, absolute/drive paths, or a Unix symlink entry.
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
        $unsafeEntry = $null
        try {
            foreach ($entry in $zip.Entries) {
                $name = $entry.FullName
                $parts = @($name -split '[\\/]')
                $unixType = (($entry.ExternalAttributes -shr 16) -band 0xF000)
                if ([string]::IsNullOrWhiteSpace($name) -or
                    [System.IO.Path]::IsPathRooted($name) -or
                    $name -match '^[a-zA-Z]:' -or
                    $parts -contains '..' -or
                    $unixType -eq 0xA000) {
                    $unsafeEntry = $name
                    break
                }
            }
        } finally {
            $zip.Dispose()
        }
        if ($null -ne $unsafeEntry) {
            Fail (Text "installer.archive_path" @{entry=$unsafeEntry})
        }
        Expand-Archive $archive -DestinationPath $tmp
        $binary = Join-Path $tmp "$Bin.exe"
        # Reject reparse points anywhere in the extracted tree, not only at
        # the final binary path.
        $link = Get-ChildItem $tmp -Recurse -Force |
            Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint } |
            Select-Object -First 1
        if ($null -ne $link -or -not (Test-Path $binary -PathType Leaf)) {
            Fail (Text "installer.archive_binary" @{bin="$Bin.exe"})
        }
        Install-Binary $binary
        return $true
    } catch {
        return $false
    } finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Install-FromSource {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Info (Text "installer.rust" @{installer="winget"})
        winget install -e --id Rustlang.Rustup `
            --accept-source-agreements --accept-package-agreements
        # Refresh PATH for this session with the freshly installed toolchain.
        $env:Path = [System.Environment]::GetEnvironmentVariable("Path", "User") +
            ";" + [System.Environment]::GetEnvironmentVariable("Path", "Machine")
    }
    Info (Text "installer.build")
    cargo install --git "https://github.com/$Repo.git" --locked
    Info (Text "installer.installed" @{bin=$Bin;dest="~/.cargo/bin"})
}

$target = Get-Target
Info (Text "installer.installing" @{repo=$Repo})
if (Install-FromRelease $target) {
    Info (Text "installer.done")
} else {
    Warn (Text "installer.source_fallback" @{target=$target})
    Install-FromSource
    Info (Text "installer.done")
}
