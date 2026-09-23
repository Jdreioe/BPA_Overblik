# Packaging and releases

One tag produces one release with one file per system (issue #8). macOS and
Linux wrap the desktop app, the CLI and the fixtures; Windows is the desktop
`.exe` itself. Nothing else has to be installed except a system Chromium.

| System | Package | Built on | Installed as |
| --- | --- | --- | --- |
| Windows 10/11 x86_64 | `.exe` (portable desktop app) | `windows-latest` | wherever the person keeps the file |
| macOS 11+ x86_64 and arm64 | `.pkg` (`pkgbuild`, not relocatable) | `macos-14` | `/Applications/Vagtplanlægning.app` |
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

Windows is a portable `.exe` so the in-app updater can replace the running file
without administrator rights. Linux does the same to the AppImage. macOS still
opens the `.pkg`, because writing into `/Applications` needs administrator
rights. The file name carries the dated release; the app reports the same value.

## Releasing

```bash
git tag v$(date -u +%Y.%m.%d)
git push origin v$(date -u +%Y.%m.%d)
```

`.github/workflows/release.yml` then builds and verifies all three packages and
publishes the release only if every platform succeeded. You can also run the
workflow manually on a selected branch. It builds that commit, then creates
today's UTC tag and publishes the release after all packages pass. A manual run
stops before building if today's tag already exists. Merging a branch does not
publish a release; pull requests that change packaging run the package checks
without publishing.

Build one package locally with `scripts/package-linux-appimage.sh`,
`scripts/package-windows-exe.ps1` or `scripts/package-macos-pkg.sh`. Each one
writes to `dist/` and runs the packaged app's `--self-check` before it reports
success.

## What CI verifies

- The workspace tests, on Linux.
- `--self-check` on the binaries that go into each package: it builds the
  representative fixture preview through the core and prints its digest.
- Windows: `--self-check` on the packaged `.exe` with the repository fixtures,
  `--help` on the built CLI, and that the run left `%APPDATA%\teamup-shift-sync\data`
  alone.
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
Windows Credential Manager prompts for nothing. The Windows `.exe` and the
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
  Signing over the `.exe` before upload.
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
decision this app should make. The packages therefore carry no browser, and
the missing-browser message names what to install.
