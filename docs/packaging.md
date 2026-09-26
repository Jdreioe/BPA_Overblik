# Packaging and releases

One tag produces one release with one file per system (issue #8), plus the
portable Windows `.exe` older releases update from. macOS and Linux wrap the
desktop app, the CLI and the fixtures; Windows installs the desktop app alone.
Nothing else has to be installed except a Chromium-based browser, and Windows
already has Edge.

| System | Package | Built on | Installed as |
| --- | --- | --- | --- |
| Windows 10/11 x86_64 | `-setup.exe` (per-user Inno Setup installer) | `windows-latest` | `%LOCALAPPDATA%\Programs\BPA Overblik\BPA Overblik.exe` |
| macOS 11+ x86_64 and arm64 | `.pkg` (`pkgbuild`, not relocatable) | `macos-14` | `/Applications/BPA Overblik.app` |
| Linux x86_64, glibc 2.35+ | `.AppImage` | `ubuntu-22.04` | wherever the person keeps the file |

The user-facing instructions live in [installation](installation.md); this file
is the maintainer's side.

## Versions are dates with an optional revision

A day's first release is `YYYY.MM.DD`, tagged `vYYYY.MM.DD`. Later releases
that day are `YYYY.MM.DD.2`, `YYYY.MM.DD.3`, and so on. The tag is the only source:
`scripts/release-version.sh` reads `TEAMUP_SHIFT_SYNC_VERSION`, which the
workflow sets from the tag, and falls back to today's date so a local packaging
run needs no edit. Crate versions in `Cargo.toml` are not release versions and
stay where they are.

The build passes the same value in as an environment variable, so the redacted
diagnostics report the installed release rather than `0.1.0`. A plain
`cargo build` still reports the crate version.

Windows is a per-user installer (`packaging/windows/bpa-overblik.iss`): no
administrator rights, a Start menu and desktop shortcut, and an entry in Apps &
features. The installed file name never carries a version. The in-app updater
downloads the next setup and, when the person clicks restart, runs it with
`/SILENT /RELAUNCH /REPLACES=<running exe>` and exits. The setup waits for that
exe to exit, installs over the previous directory and starts the new version.
The `AppId` in the script is how an update finds the existing install and must
never change.

Releases before the installer were a portable `teamup-shift-sync-*-x86_64.exe`
whose updater only knows that asset name, so every release still publishes it.
Such a copy first updates in place to the portable build of the new release;
that build's updater then offers the setup, and the setup deletes the portable
file (only a file with that name pattern) once the installed copy is in place.

Linux replaces the running AppImage in place. macOS still
opens the `.pkg`, because writing into `/Applications` needs administrator
rights. The file name carries the dated release; the app reports the same value.
The macOS package and bundle use three numeric components: year, month and day
combined, and the day's release number. For example, `2026.09.24.2` becomes
`2026.924.2`. This keeps later dates and revisions increasing for macOS.

## Releasing

Run the Release workflow manually on the commit to release. It selects today's
UTC date and the next unused revision, builds all three packages, then creates
the tag and publishes the release after every platform passes. Manual runs are
serialized so they cannot choose the same tag. You can also push an unused tag
in the format above to start the same build. Merging a branch does not
publish a release; pull requests that change packaging run the package checks
without publishing.

Build one package locally with `scripts/package-linux-appimage.sh`,
`scripts/package-windows-exe.ps1` (needs Inno Setup 6, `winget install
JRSoftware.InnoSetup`) or `scripts/package-macos-pkg.sh`. Each one
writes to `dist/` and runs the packaged app's `--self-check` before it reports
success.

## What CI verifies

- The workspace tests, on Linux.
- `--self-check` on the binaries that go into each package: it builds the
  representative fixture preview through the core and prints its digest.
- Windows: a silent install that deletes a stand-in portable exe and nothing
  else, the Apps & features version, `--self-check` on the installed `.exe`
  with the repository fixtures, `--help` on the built CLI, a silent uninstall
  that removes the app and its entry, and that all of it left
  `%APPDATA%\teamup-shift-sync\data` alone.
- macOS: `installer -pkg`, that the installed binary is universal
  (`x86_64 arm64`), `--self-check` and `--help` from `/Applications`, and that
  the installed package left the data directory alone.
- Linux: `--self-check` through the finished AppImage.

What CI does not cover: a real login to MitHF or DUOS, a live transfer, screen
readers, and the upgrade path over a genuinely older installed version. Those
remain manual checks in [desktop validation](desktop-validation.md).

## User data is never packaged, never removed

Setup, credentials handles and sync history live outside the package:

- Windows: `%APPDATA%\teamup-shift-sync\data`
- macOS: `~/Library/Application Support/teamup-shift-sync`
- Linux: `~/.local/share/teamup-shift-sync`

No package writes there, and deleting or replacing the program does not delete
it. Passwords themselves are in the OS credential store, which the packages
never touch either.

The only OS permission the app needs is access to that credential store:
macOS prompts for keychain access on first save, Linux needs an unlocked
desktop keyring (GNOME Keyring or equivalent through Secret Service), and
Windows Credential Manager prompts for nothing. The Windows installer and the
Linux AppImage need no administrator rights. The macOS `.pkg` still asks once
to install into `/Applications`.

The macOS package is deliberately not relocatable. A relocatable bundle is
installed over whatever copy Launch Services knows about, which in CI meant the
build directory instead of `/Applications`, so an upgrade could land anywhere
the person once kept a copy.

## Signing and notarization

Nothing is signed yet, so Windows SmartScreen and macOS Gatekeeper both ask
once. [installation](installation.md) documents the exact clicks.

Removing those prompts needs paid identities, not code:

- Windows: an EV or Azure Trusted Signing certificate, then `signtool`/Trusted
  Signing over the GUI `.exe` before Inno Setup packs it and over the setup
  `.exe` before upload.
- macOS: an Apple Developer ID Application and Installer certificate, then
  `codesign --options runtime` on the app, `productsign` on the pkg, and
  `xcrun notarytool submit --wait` plus `xcrun stapler staple`.

Both fit as extra steps in the existing platform jobs, reading certificates
from repository secrets. Until then, the release notes say plainly that the
packages are unsigned.

## Browser distribution

The app requires an installed Chromium-based browser (Edge, Chrome, Chromium or
Brave) or `TEAMUP_BROWSER_PATH` and never downloads one. That stays true in the packages: shipping a browser would multiply the
download size, and silently downloading one on a care worker's machine is not a
decision this app should make. The packages therefore carry no browser, and
the missing-browser message names what to install.
