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
        $archive = Join-Path $tmp $asset
        Invoke-WebRequest -Uri $url -OutFile $archive
        # The checksum is mandatory. Missing, malformed, misnamed, and
        # mismatched checksum files all abort the release install.
        $checksumFile = Join-Path $tmp "$asset.sha256"
        try {
            Invoke-WebRequest -Uri "$url.sha256" -OutFile $checksumFile
        } catch {
            Fail "required checksum unavailable for $asset - aborting"
        }
        Info "verifying checksum"
        $checksumLines = @(Get-Content $checksumFile |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if ($checksumLines.Count -ne 1) {
            Fail "invalid checksum file for $asset - aborting"
        }
        $checksumLine = $checksumLines[0]
        $checksumParts = @($checksumLine.Trim() -split '\s+')
        if ($checksumParts.Count -ne 2 -or
            $checksumParts[0] -notmatch '^[0-9a-fA-F]{64}$' -or
            $checksumParts[1].TrimStart('*') -ne $asset) {
            Fail "invalid checksum file for $asset - aborting"
        }
        $expected = $checksumParts[0].ToLowerInvariant()
        $actual = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($expected -ne $actual) {
            Fail "checksum mismatch for $asset - aborting"
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
            Fail "unsafe path or link in release archive: $unsafeEntry"
        }
        Expand-Archive $archive -DestinationPath $tmp
        $binary = Join-Path $tmp "$Bin.exe"
        # Reject reparse points anywhere in the extracted tree, not only at
        # the final binary path.
        $link = Get-ChildItem $tmp -Recurse -Force |
            Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint } |
            Select-Object -First 1
        if ($null -ne $link -or -not (Test-Path $binary -PathType Leaf)) {
            Fail "release archive did not contain a regular $Bin.exe file"
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
