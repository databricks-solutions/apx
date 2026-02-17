# apx installer script for Windows
# Usage: irm https://databricks-solutions.github.io/apx/install.ps1 | iex
#
# Options (when dot-sourced or invoked directly):
#   -Version <tag>     Install a specific version (default: latest)
#   -NoModifyPath      Don't modify the user PATH
#   -InstallDir <dir>  Override installation directory

param(
    [string]$Version = "",
    [switch]$NoModifyPath,
    [string]$InstallDir = ""
)

$ErrorActionPreference = "Stop"

$Repo = "databricks-solutions/apx"
$GitHubApi = "https://api.github.com"
$GitHubReleases = "https://github.com/$Repo/releases/download"

# ---------------------------------------------------------------------------
# Color helpers
# ---------------------------------------------------------------------------
function Write-Info {
    param([string]$Message)
    Write-Host "info: " -ForegroundColor Cyan -NoNewline
    Write-Host $Message
}

function Write-Warn {
    param([string]$Message)
    Write-Host "warn: " -ForegroundColor Yellow -NoNewline
    Write-Host $Message
}

function Write-Err {
    param([string]$Message)
    Write-Host "error: " -ForegroundColor Red -NoNewline
    Write-Host $Message
}

function Write-Ok {
    param([string]$Message)
    Write-Host "success: " -ForegroundColor Green -NoNewline
    Write-Host $Message
}

# ---------------------------------------------------------------------------
# Check for existing installation
# ---------------------------------------------------------------------------
$existing = Get-Command apx -ErrorAction SilentlyContinue
if ($existing) {
    $existingVersion = & apx --version 2>$null
    Write-Warn "apx is already installed: $existingVersion"
    Write-Warn "Run 'apx upgrade' to update, or remove the existing installation first."
    return
}

# ---------------------------------------------------------------------------
# Determine install directory
# ---------------------------------------------------------------------------
if (-not $InstallDir) {
    if ($env:APX_INSTALL_DIR) {
        $InstallDir = $env:APX_INSTALL_DIR
    } elseif ($env:XDG_BIN_HOME) {
        $InstallDir = $env:XDG_BIN_HOME
    } elseif ($env:XDG_DATA_HOME) {
        $InstallDir = Join-Path (Split-Path $env:XDG_DATA_HOME -Parent) "bin"
    } else {
        $InstallDir = Join-Path $env:USERPROFILE ".local\bin"
    }
}

Write-Info "Install directory: $InstallDir"

# ---------------------------------------------------------------------------
# Resolve version
# ---------------------------------------------------------------------------
if (-not $Version) {
    Write-Info "Fetching latest release..."
    try {
        $release = Invoke-RestMethod -Uri "$GitHubApi/repos/$Repo/releases/latest" -Headers @{ "User-Agent" = "apx-installer" }
        $Version = $release.tag_name
    } catch {
        Write-Err "Failed to fetch latest release from GitHub: $_"
        return
    }
}

Write-Info "Version: $Version"

# ---------------------------------------------------------------------------
# Download
# ---------------------------------------------------------------------------
$AssetName = "apx-x86_64-windows.exe"
$Url = "$GitHubReleases/$Version/$AssetName"
$DestPath = Join-Path $InstallDir "apx.exe"

Write-Info "Downloading $Url..."

if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

try {
    Invoke-WebRequest -Uri $Url -OutFile $DestPath -UseBasicParsing
} catch {
    Write-Err "Download failed. Check that version '$Version' exists."
    Write-Err "URL: $Url"
    return
}

Write-Ok "Installed apx to $DestPath"

# ---------------------------------------------------------------------------
# PATH modification
# ---------------------------------------------------------------------------
if (-not $NoModifyPath) {
    $currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($currentPath -notlike "*$InstallDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$InstallDir;$currentPath", "User")
        $env:Path = "$InstallDir;$env:Path"
        Write-Info "Added $InstallDir to user PATH"
        Write-Warn "Restart your terminal for PATH changes to take effect."
    }
}

# ---------------------------------------------------------------------------
# Dependency checks
# ---------------------------------------------------------------------------
if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
    Write-Info "uv not found on PATH. apx will download it automatically on first use."
}

if (-not (Get-Command databricks -ErrorAction SilentlyContinue)) {
    Write-Warn "Databricks CLI is not installed. Some apx features require it."
    Write-Warn "Install it from: https://docs.databricks.com/aws/en/dev-tools/cli/install"
}

# ---------------------------------------------------------------------------
# Success banner
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "apx $Version installed successfully!" -ForegroundColor Green -Bold
Write-Host ""
Write-Host "  Binary:  " -ForegroundColor Cyan -NoNewline
Write-Host $DestPath
Write-Host ""
Write-Host "  Get started:"
Write-Host "    apx init        " -NoNewline -ForegroundColor White
Write-Host "Create a new project"
Write-Host "    apx dev start   " -NoNewline -ForegroundColor White
Write-Host "Start development server"
Write-Host ""
