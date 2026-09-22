# Build the Windows package: one portable desktop .exe. Needs a stable Rust
# toolchain. The CLI is still built and checked, but the file people download
# is the GUI. User data stays in %APPDATA%\teamup-shift-sync, not beside the exe.
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $root

$version = $env:TEAMUP_SHIFT_SYNC_VERSION
if (-not $version) { $version = (Get-Date).ToUniversalTime().ToString("yyyy.MM.dd") }
if ($version -notmatch '^\d{4}\.\d{2}\.\d{2}$') {
    throw "Release version must be YYYY.MM.DD, got: $version"
}

Write-Host "Building Vagtplanlægning $version for Windows/x86_64"

$target = "x86_64-pc-windows-msvc"
$env:TEAMUP_SHIFT_SYNC_VERSION = $version
rustup target add $target
cargo build --release --locked --workspace --target $target
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

Write-Host "Verifying the built binaries"
$env:TEAMUP_SHIFT_SYNC_CONFIG = Join-Path $root "fixtures\offline-config.toml"
$env:TEAMUP_FIXTURE = Join-Path $root "fixtures\representative-week.json"
$gui = "target\$target\release\teamup-shift-sync-gui.exe"
# A release GUI build is a windowed app, so `&` returns without waiting and
# $LASTEXITCODE keeps whatever the previous native command left. Wait for the
# process and read its exit code instead.
$stdout = Join-Path ([System.IO.Path]::GetTempPath()) "teamup-selfcheck.log"
$stderr = Join-Path ([System.IO.Path]::GetTempPath()) "teamup-selfcheck.err.log"
$selfcheck = Start-Process -FilePath $gui -ArgumentList "--self-check" -NoNewWindow -Wait -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
Get-Content $stdout
if ($selfcheck.ExitCode -ne 0) { Get-Content $stderr; throw "self-check failed" }
& "target\$target\release\teamup-shift-sync-rust.exe" --help | Out-Null
if ($LASTEXITCODE -ne 0) { throw "CLI failed to start" }
Remove-Item Env:TEAMUP_SHIFT_SYNC_CONFIG, Env:TEAMUP_FIXTURE

New-Item -ItemType Directory -Force "dist" | Out-Null
$exe = "dist\teamup-shift-sync-$version-x86_64.exe"
Copy-Item $gui $exe -Force

Write-Host "Windows package: $exe"
