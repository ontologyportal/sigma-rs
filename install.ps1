<#
.SYNOPSIS
    One-shot installer for the `sumo` CLI (sigmakee-rs) on Windows.

.DESCRIPTION
    Downloads the latest `sigmakee-v*` GitHub release (win32-x64), installs
    it under $SIGMA_HOME\bin, sets SIGMA_HOME + PATH persistently for the
    current user, and generates a starter config.xml. Mirrors install.sh's
    behavior for macOS/Linux.

.EXAMPLE
    irm https://raw.githubusercontent.com/ontologyportal/sigma-rs/main/install.ps1 | iex

.NOTES
    Env overrides (set before running):
      $env:SIGMA_HOME    Install root (default: $HOME\.sigmakee)
      $env:SUMO_VERSION  Specific release tag, e.g. sigmakee-v2.0.0
                          (default: the latest sigmakee-v* release)
#>

$ErrorActionPreference = 'Stop'

if ($PSVersionTable.PSEdition -eq 'Core' -and -not $IsWindows) {
    Write-Host "error: this installer is for Windows only (win32-x64 release asset)." -ForegroundColor Red
    return
}

$RepoOwner = 'ontologyportal'
$RepoName  = 'sigma-rs'
$TagPrefix = 'sigmakee-v'

$SigmaHome = if ($env:SIGMA_HOME) { $env:SIGMA_HOME } else { Join-Path $HOME '.sigmakee' }
$BinDir    = Join-Path $SigmaHome 'bin'
$SumoExe   = Join-Path $BinDir 'sumo.exe'

function Info($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Warn($msg) { Write-Warning $msg }

# Resolve the release tag
# GitHub's release list is newest-first, and this repo publishes releases
# under more than one tag prefix (e.g. `sumo-lsp-v*` for a different crate),
# so the global "latest release" is NOT necessarily a sumo release
function Resolve-Tag {
    if ($env:SUMO_VERSION) { return $env:SUMO_VERSION }

    $releases = Invoke-RestMethod -Uri "https://api.github.com/repos/$RepoOwner/$RepoName/releases" -UseBasicParsing
    $match = $releases | Where-Object { $_.tag_name -like "$TagPrefix*" } | Select-Object -First 1
    if (-not $match) {
        throw "Could not find a $TagPrefix* release — check https://github.com/$RepoOwner/$RepoName/releases"
    }
    return $match.tag_name
}

# Download, verify, install the binary
function Install-Binary($Tag) {
    $Ver     = $Tag -replace "^$TagPrefix", ''
    $Staging = "sumo-$Ver-win32-x64"
    $Archive = "$Staging.zip"
    $BaseUrl = "https://github.com/$RepoOwner/$RepoName/releases/download/$Tag"

    $Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Path $Tmp -Force | Out-Null
    $ArchivePath = Join-Path $Tmp $Archive
    $ShaPath     = Join-Path $Tmp "$Archive.sha256"

    try {
        Info "Downloading $Archive ($Tag)..."
        try {
            Invoke-WebRequest -Uri "$BaseUrl/$Archive" -OutFile $ArchivePath -UseBasicParsing
            Invoke-WebRequest -Uri "$BaseUrl/$Archive.sha256" -OutFile $ShaPath -UseBasicParsing
        } catch {
            throw "Download failed: $_"
        }

        Info "Verifying checksum..."
        $expected = ((Get-Content $ShaPath) -split '\s+')[0].ToLower()
        $actual   = (Get-FileHash -Path $ArchivePath -Algorithm SHA256).Hash.ToLower()
        if ($expected -ne $actual) {
            throw "Checksum verification failed — the download may be corrupt"
        }

        Expand-Archive -Path $ArchivePath -DestinationPath $Tmp -Force
        $ExtractedExe = Join-Path (Join-Path $Tmp $Staging) 'sumo.exe'
        if (-not (Test-Path $ExtractedExe)) {
            throw "Archive didn't contain the expected sumo.exe binary"
        }

        New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
        Copy-Item -Path $ExtractedExe -Destination $SumoExe -Force
        Info "Installed sumo $Ver -> $SumoExe"
    } finally {
        Remove-Item -Path $Tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# Set SIGMA_HOME + PATH persistently (current user)
# Windows has no ~/.bashrc equivalent — persistent env vars live in the
# registry, which SetEnvironmentVariable(..., 'User') writes directly; no
# idempotency-marker/file-editing logic needed (re-running just overwrites
# the same two values). Also mirrored into $env: for THIS process, so the
# config-generation step below can run `sumo` immediately without needing a
# new terminal — the registry changes themselves only apply to *new* shells.
function Set-SumoEnvironment {
    [Environment]::SetEnvironmentVariable('SIGMA_HOME', $SigmaHome, 'User')
    $userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
    if ($userPath -notlike "*$BinDir*") {
        $newPath = if ([string]::IsNullOrEmpty($userPath)) { $BinDir } else { "$BinDir;$userPath" }
        [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
    }
    Info "Set SIGMA_HOME and updated PATH for your user account."

    $env:SIGMA_HOME = $SigmaHome
    if ($env:PATH -notlike "*$BinDir*") { $env:PATH = "$BinDir;$env:PATH" }
}

# Generate a starter config.xml
function New-Config {
    Info 'Generating $SIGMA_HOME\KBs\config.xml...'
    # A flag forces `sumo config`'s write mode deterministically — bare
    # `sumo config` launches the interactive TUI in a real terminal (wrong
    # for an unattended installer) or just prints a dump otherwise (never
    # writes anything). --base-dir seeds the one setting that actually
    # matters at install time; every other field gets its built-in default.
    & $SumoExe config --base-dir $SigmaHome

    # Older releases (before `sumo config` gained a write mode) accept the
    # flag but only ever print a dump — exit 0 either way, so the only real
    # signal is whether the file actually landed.
    $ConfigPath = Join-Path $SigmaHome 'KBs\config.xml'
    if (-not (Test-Path $ConfigPath)) {
        Warn "config.xml was not created — this sumo build may predate 'sumo config' write support."
        Warn "Create it by hand once a newer release is available: sumo config --base-dir `"$SigmaHome`""
    }
}

function Main {
    $Tag = Resolve-Tag
    Install-Binary -Tag $Tag
    Set-SumoEnvironment
    New-Config

    Write-Host ''
    Info "Done. Installed: $(& $SumoExe --version)"
    Info 'Open a new terminal for SIGMA_HOME/PATH to take effect there (this session already has them).'
    Info 'config.xml has no <kb> configured yet — see README.md''s Quick start'
    Info 'for loading an ontology (e.g. `sumo --git <repo> --branch <name> load`).'
}

try {
    Main
} catch {
    Write-Host "error: $($_.Exception.Message)" -ForegroundColor Red
}
