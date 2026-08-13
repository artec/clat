# CLAT installer for Windows (PowerShell).
#
# Usage:
#   irm https://raw.githubusercontent.com/artec/clat/main/install.ps1 | iex
#
# Detects the architecture, prefers a prebuilt binary from GitHub
# Releases, and falls back to building from source when no release exists
# yet (offering to install the Rust toolchain if cargo is missing).
$ErrorActionPreference = "Stop"

$Repo = "artec/clat"
$Bin = "clat"

function Info($m) { Write-Host "> $m" -ForegroundColor Cyan }
function Warn($m) { Write-Host "! $m" -ForegroundColor Yellow }
function Fail($m) { Write-Host "x $m" -ForegroundColor Red; exit 1 }

function Get-Target {
    switch ($env:PROCESSOR_ARCHITECTURE) {
        "ARM64" { return "aarch64-pc-windows-msvc" }
        "AMD64" { return "x86_64-pc-windows-msvc" }
        default { Fail "unsupported architecture: $($env:PROCESSOR_ARCHITECTURE)" }
    }
}

function Install-Binary($file) {
    $dest = Join-Path $env:LOCALAPPDATA "clat\bin"
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Copy-Item $file (Join-Path $dest "$Bin.exe") -Force
    Info "installed $Bin.exe to $dest"
    if (-not (($env:Path -split ';') -contains $dest)) {
        Warn "add $dest to your user PATH, for example:  setx PATH `"$($env:Path);$dest`""
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
        Info "downloading $url"
        Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp $asset)
        Expand-Archive (Join-Path $tmp $asset) -DestinationPath $tmp
        Install-Binary (Join-Path $tmp "$Bin.exe")
        return $true
    } catch {
        return $false
    } finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Install-FromSource {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Info "Rust is required to build from source; installing via winget"
        winget install -e --id Rustlang.Rustup `
            --accept-source-agreements --accept-package-agreements
        # Refresh PATH for this session with the freshly installed toolchain.
        $env:Path = [System.Environment]::GetEnvironmentVariable("Path", "User") +
            ";" + [System.Environment]::GetEnvironmentVariable("Path", "Machine")
    }
    Info "building CLAT from source (takes a few minutes)"
    cargo install --git "https://github.com/$Repo.git" --locked
    Info "installed $Bin to ~/.cargo/bin"
}

$target = Get-Target
Info "installing CLAT (github.com/$Repo) for $target"
if (Install-FromRelease $target) {
    Info "done"
} else {
    Warn "no prebuilt release for $target — building from source"
    Install-FromSource
    Info "done"
}
