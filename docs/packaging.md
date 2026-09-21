# Packaging and releases

One tag produces one release with one installer per system (issue #8). The
installers carry the desktop app, the CLI and the fixtures; nothing else has to
be installed except a system Chromium.

| System | Package | Built on | Installed as |
| --- | --- | --- | --- |
| Windows 10/11 x86_64 | `.msi` (WiX, per-machine, no wizard) | `windows-latest` | `C:\Program Files\Vagtplanlægning` + Start menu |
| macOS 11+ x86_64 and arm64 | `.pkg` (`productbuild`) | `macos-14` | `/Applications/Vagtplanlægning.app` |
| Linux x86_64, glibc 2.35+ | `.AppImage` | `ubuntu-22.04` | wherever the person keeps the file |

The user-facing instructions live in [installation](installation.md); this file
is the maintainer's side.

## Versions are dates

A release is `YYYY.MM.DD`, tagged `vYYYY.MM.DD`. The tag is the only source:
`scripts/release-version.sh` reads `TEAMUP_SHIFT_SYNC_VERSION`, which the
workflow sets from the tag, and falls back to today's date so a local packaging
run needs no edit. Crate versions in `Cargo.toml` are not release versions and
stay where they are.

The build passes the same value in as an environment variable, so the redacted
diagnostics report the installed release rather than `0.1.0`. A plain
`cargo build` still reports the crate version.

Windows Installer only allows 0-255 in the first version field, so the MSI
version is `YY.M.D`: the release `2026.09.21` installs as `26.9.21` in
Programs and Features. Ordering is preserved, and the file name and the app
both show the full date.

## Releasing

```bash
git tag v$(date -u +%Y.%m.%d)
git push origin v$(date -u +%Y.%m.%d)
```

`.github/workflows/release.yml` then builds and verifies all three packages and
publishes the release only if every platform succeeded. Running the workflow
manually builds and verifies the same packages with today's date and publishes
nothing, which is how a packaging change is checked before tagging.

Build one package locally with `scripts/package-linux-appimage.sh`,
`scripts/package-windows-msi.ps1` or `scripts/package-macos-pkg.sh`. Each one
writes to `dist/` and runs the packaged app's `--self-check` before it reports
success.

## What CI verifies

- The workspace tests, on Linux.
- `--self-check` on the binaries that go into each package: it builds the
  representative fixture preview through the core and prints its digest.
- Windows: a silent install of the MSI, `--self-check` and `--help` from the
  installed location, a silent uninstall, and that the uninstall removed the
  program but kept `%APPDATA%\teamup-shift-sync\data`.
- macOS: `installer -pkg`, that the installed binary is universal
  (`x86_64 arm64`), `--self-check` and `--help` from `/Applications`, and that
  the installed package left the data directory alone.
- Linux: `--self-check` through the finished AppImage.

What CI does not cover: a real login to MitHF or DUOS, a live transfer, screen
readers, and the upgrade path over a genuinely older installed version. Those
remain manual checks in [desktop validation](desktop-validation.md).

## User data is never packaged, never removed

Setup, credentials handles and sync history live outside the install location:

- Windows: `%APPDATA%\teamup-shift-sync\data`
- macOS: `~/Library/Application Support/teamup-shift-sync`
- Linux: `~/.local/share/teamup-shift-sync`

No installer writes there, and no uninstaller deletes it. Passwords themselves
are in the OS credential store, which the installers never touch either.

The only OS permission the app needs is access to that credential store:
macOS prompts for keychain access on first save, Linux needs an unlocked
desktop keyring (GNOME Keyring or equivalent through Secret Service), and
Windows Credential Manager prompts for nothing. Installing per-machine on
Windows needs administrator rights once; nothing after that does.

## Signing and notarization

Nothing is signed yet, so Windows SmartScreen and macOS Gatekeeper both ask
once. [installation](installation.md) documents the exact clicks.

Removing those prompts needs paid identities, not code:

- Windows: an EV or Azure Trusted Signing certificate, then `signtool`/Trusted
  Signing over both the `.exe` files and the `.msi` before upload.
- macOS: an Apple Developer ID Application and Installer certificate, then
  `codesign --options runtime` on the app, `productsign` on the pkg, and
  `xcrun notarytool submit --wait` plus `xcrun stapler staple`.

Both fit as extra steps in the existing platform jobs, reading certificates
from repository secrets. Until then, the release notes say plainly that the
packages are unsigned.

## Browser distribution

The app requires a system Chromium or `TEAMUP_BROWSER_PATH` and never downloads
one. That stays true in the packages: shipping a browser would multiply the
download size, and silently downloading one on a care worker's machine is not a
decision this app should make. The installers therefore carry no browser, and
the missing-browser message names what to install.
