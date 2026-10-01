# bvr installer -- Beads Viewer in Rust (Windows)
# Usage: irm "https://raw.githubusercontent.com/quangdang46/beads_viewer_rust/main/install.ps1" | iex
$ErrorActionPreference = "Stop"
# Disables the slow IE-style progress bar in Invoke-WebRequest, which can
# slow large downloads from seconds to minutes.
$ProgressPreference = "SilentlyContinue"

# Force TLS 1.2 (and 1.3 if available). Windows PowerShell 5.1 still defaults
# to TLS 1.0/1.1 for .NET HTTP clients, which GitHub now rejects or which can
# silently truncate a download mid-stream -- surfacing here as a checksum
# mismatch rather than a connection error. The -bor preserves any newer
# protocols the runtime already has enabled.
try {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch { }

# === Config ===
$BinaryName = "bvr"
$BinaryExe  = "bvr.exe"
$Owner      = "quangdang46"
$Repo       = "beads_viewer_rust"
$Dest       = if ($env:DEST) { $env:DEST } else { Join-Path $env:USERPROFILE ".local\bin" }
$Version    = $env:VERSION
$EasyMode   = $false
$FromSource = $false
$MaxRetries = 3

function Log-Info($msg)    { Write-Host "[$BinaryName] $msg" }
function Log-Warn($msg)    { Write-Host "[$BinaryName] WARN: $msg" -ForegroundColor Yellow }
function Die($msg)         { Write-Host "ERROR: $msg" -ForegroundColor Red; exit 1 }

# === Args ===
foreach ($arg in $args) {
    switch ($arg) {
        "--easy-mode"   { $EasyMode = $true }
        "--from-source" { $FromSource = $true }
        default {}
    }
}

# === Platform (Split with limit 2 -- windows_x86_64 must keep _64) ===
function Get-Platform {
    $arch = switch ($env:PROCESSOR_ARCHITECTURE) {
        "AMD64" { "x86_64" }
        "ARM64" { "aarch64" }
        default { Die "Unsupported arch: $env:PROCESSOR_ARCHITECTURE" }
    }
    return "windows-$arch"
}

# === Version resolution ===
function Resolve-Version {
    if ($Version) { return }
    try {
        $script:Version = (Invoke-RestMethod -Uri "https://api.github.com/repos/$Owner/$Repo/releases/latest" `
            -TimeoutSec 30 -UseBasicParsing).tag_name
    } catch {
        Die "Could not resolve latest version (no releases yet?). Use --from-source or install Rust and build."
    }
}

# === From source ===
function Build-FromSource {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Die "cargo not found -- install Rust first: https://rustup.rs"
    }
    $src = Join-Path $env:TEMP "bvr-src-$(Get-Random)"
    git clone --depth 1 "https://github.com/$Owner/$Repo.git" $src
    Push-Location $src
    try {
        cargo build --release -p bv
        $bin = Join-Path $src "target\release\$BinaryExe"
        if (-not (Test-Path $bin)) { Die "Build finished but binary not found at $bin" }
        New-Item -ItemType Directory -Force -Path $Dest | Out-Null
        Copy-Item $bin (Join-Path $Dest $BinaryExe) -Force
    } finally { Pop-Location }
}

# === Main ===
if ($FromSource) {
    Build-FromSource
} else {
    Resolve-Version
    Log-Info "Latest release: $Version"
    $platform = Get-Platform
    $archive  = "$BinaryName-$Version-$platform.zip"
    $url      = "https://github.com/$Owner/$Repo/releases/download/$Version/$archive"
    $tmpZip   = Join-Path $env:TEMP "$archive"
    $tmpDir   = Join-Path $env:TEMP "bvr-extract-$(Get-Random)"

    # Fetch the expected checksum once (independent of the archive itself,
    # so a corrupted/retried archive download can't also poison this value).
    $expected = $null
    try {
        $sumResp = Invoke-WebRequest -Uri "$url.sha256" -TimeoutSec 60 -UseBasicParsing -ErrorAction Stop
        # GitHub serves this sidecar as application/octet-stream, so
        # Invoke-WebRequest can't infer a text encoding and hands back
        # .Content as a raw byte[] instead of a string -- splitting that on
        # whitespace yields individual bytes-as-decimal (e.g. "102" for 'f'),
        # not the hash. Decode explicitly regardless of what type we got.
        if ($sumResp.Content -is [byte[]]) {
            $sumText = [System.Text.Encoding]::UTF8.GetString($sumResp.Content)
        } else {
            $sumText = $sumResp.Content
        }
        $expected = ($sumText -split '\s+')[0].ToLower()
    } catch { Log-Warn "Could not fetch .sha256 sidecar ($($_.Exception.Message)) -- skipping verification" }

    $ok = $false
    for ($i = 1; $i -le $MaxRetries; $i++) {
        Remove-Item $tmpZip -ErrorAction SilentlyContinue
        try {
            Invoke-WebRequest -Uri $url -OutFile $tmpZip -TimeoutSec 120 -UseBasicParsing -ErrorAction Stop
        } catch {
            Log-Warn "Download attempt $i/$MaxRetries failed: $($_.Exception.Message)"; Start-Sleep 3; continue
        }
        if (-not $expected) { $ok = $true; break }
        $actual = (Get-FileHash $tmpZip -Algorithm SHA256).Hash.ToLower()
        if ($actual -eq $expected) { Log-Info "Checksum verified"; $ok = $true; break }
        $size = (Get-Item $tmpZip).Length
        Log-Warn "Checksum mismatch on attempt $i/$MaxRetries (expected $expected, got $actual, size $size bytes) -- retrying..."
        Start-Sleep 3
    }
    if (-not $ok) {
        Log-Warn "Download kept failing verification after $MaxRetries attempts."
        Log-Warn "This usually means something between you and GitHub (proxy/AV/VPN) is altering the download, not that the release is broken."
        Log-Warn "Falling back to building from source..."
        Build-FromSource
    } else {
        Expand-Archive -Path $tmpZip -DestinationPath $tmpDir -Force
        $bin = Get-ChildItem -Path $tmpDir -Recurse -Filter $BinaryExe | Select-Object -First 1
        if (-not $bin) { Die "Binary not found after extract" }
        New-Item -ItemType Directory -Force -Path $Dest | Out-Null
        Copy-Item $bin.FullName (Join-Path $Dest $BinaryExe) -Force
    }
}

# === PATH ===
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ";") -notcontains $Dest) {
    if ($EasyMode) {
        [Environment]::SetEnvironmentVariable("Path", "$Dest;$userPath", "User")
        Log-Warn "PATH updated -- restart your terminal."
    } else {
        Log-Warn "Add to PATH manually: $Dest"
    }
}

Write-Host ""
Write-Host ("{0} installed -> {1}" -f $BinaryName, (Join-Path $Dest $BinaryExe)) -ForegroundColor Green
Write-Host "  Quick start: cd your-beads-project && bvr"
