# Build the Windows packages: a per-user installer (the file people download)
# and the portable desktop .exe. Needs a stable Rust toolchain and Inno Setup 6.
# The portable .exe is still published because releases from before the
# installer update themselves by replacing it; their next update then moves to
# the installer. The CLI is still built and checked. User data stays in
# %APPDATA%\teamup-shift-sync, not beside the exe.
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $root

$version = $env:TEAMUP_SHIFT_SYNC_VERSION
if (-not $version) { $version = (Get-Date).ToUniversalTime().ToString("yyyy.MM.dd") }
if ($version -cnotmatch '^[0-9]{4}\.[0-9]{2}\.[0-9]{2}(\.[2-9]|\.[1-9][0-9]+)?$') {
    throw "Release version must be YYYY.MM.DD or YYYY.MM.DD.N (N >= 2), got: $version"
}

Write-Host "Building BPA Overblik $version for Windows/x86_64"

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

$iscc = (Get-Command "ISCC.exe" -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    $iscc = @(
        (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
        (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe"),
        (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe")
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $iscc) { throw "Inno Setup 6 is required: winget install JRSoftware.InnoSetup" }

# Windows file versions are four numbers without leading zeros; a day's first
# release is revision 1.
$parts = $version.Split(".")
$revision = if ($parts.Count -eq 4) { $parts[3] } else { "1" }
$versionInfo = "{0}.{1}.{2}.{3}" -f [int]$parts[0], [int]$parts[1], [int]$parts[2], [int]$revision

Write-Host "Building the installer with $iscc"
& $iscc /Qp "/DAppVersion=$version" "/DVersionInfo=$versionInfo" `
    "/DSourceExe=$(Resolve-Path $gui)" "/DOutputDir=$(Resolve-Path dist)" `
    "packaging\windows\bpa-overblik.iss"
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed" }
$setup = "dist\teamup-shift-sync-$version-x86_64-setup.exe"
if (-not (Test-Path $setup)) { throw "Inno Setup did not write $setup" }

Write-Host "Windows installer: $setup"
Write-Host "Windows portable: $exe"
