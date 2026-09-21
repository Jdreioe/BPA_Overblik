# Build the Windows installer: one per-machine MSI with the desktop app, the
# CLI and the fixtures. Needs a stable Rust toolchain and the .NET SDK, which
# provides the WiX tool.
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $root

$version = $env:TEAMUP_SHIFT_SYNC_VERSION
if (-not $version) { $version = (Get-Date).ToUniversalTime().ToString("yyyy.MM.dd") }
if ($version -notmatch '^\d{4}\.\d{2}\.\d{2}$') {
    throw "Release version must be YYYY.MM.DD, got: $version"
}

# Windows Installer only allows 0-255 in the first version field, so the dated
# release 2026.09.21 becomes the MSI version 26.9.21. Ordering is preserved.
$parts = $version.Split('.')
$msiVersion = "{0}.{1}.{2}" -f ([int]$parts[0] - 2000), [int]$parts[1], [int]$parts[2]

Write-Host "Building Vagtplanlægning $version for Windows/x86_64 (MSI version $msiVersion)"

$target = "x86_64-pc-windows-msvc"
$env:TEAMUP_SHIFT_SYNC_VERSION = $version
rustup target add $target
cargo build --release --locked --workspace --target $target
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

Write-Host "Verifying the built binaries"
$env:TEAMUP_SHIFT_SYNC_CONFIG = Join-Path $root "fixtures\offline-config.toml"
$env:TEAMUP_FIXTURE = Join-Path $root "fixtures\representative-week.json"
& "target\$target\release\teamup-shift-sync-gui.exe" --self-check
if ($LASTEXITCODE -ne 0) { throw "self-check failed" }
Remove-Item Env:TEAMUP_SHIFT_SYNC_CONFIG, Env:TEAMUP_FIXTURE

if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    dotnet tool install --global wix --version 6.0.2
    $tools = Join-Path $env:USERPROFILE ".dotnet\tools"
    if ($env:PATH -notlike "*$tools*") { $env:PATH = "$env:PATH;$tools" }
}
New-Item -ItemType Directory -Force "dist" | Out-Null
$msi = "dist\teamup-shift-sync-$version-x86_64.msi"
Remove-Item $msi -ErrorAction SilentlyContinue

wix build `
    -arch x64 `
    -culture da-DK `
    -d "PackageVersion=$msiVersion" `
    -d "DisplayVersion=$version" `
    -pdbtype none `
    -o $msi `
    packaging\windows\teamup-shift-sync.wxs
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }

Write-Host "Windows installer: $msi"
