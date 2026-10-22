# Releasing MailCraft

Releases are built by GitHub Actions on GitHub-hosted runners (`.github/workflows/release.yml`) and
published as a [GitHub release](https://github.com/MonkeySaint/mailcraft/releases) with every package and a
`SHA256SUMS` file. Nothing needs registering: the workflows run as soon as they are on GitHub. Everything in
this document can also be run by hand on a developer machine.

## Cutting a release

1. Make sure `master` is green: `cargo xtask ci`.
2. Bump the version (root `Cargo.toml` `[workspace.package]` and the workspace entries in `Cargo.lock`):

   ```sh
   cargo xtask version bump 0.2.0
   ```

3. Commit and tag. The tag must be `v` + the workspace version; the `version-check` job fails otherwise.

   ```sh
   git commit -am "Release 0.2.0"
   git tag -a v0.2.0 -m "MailCraft 0.2.0"
   ```

4. Push the commit, then the tag to `origin` (`git@github.com:MonkeySaint/mailcraft.git`; pushing is the
   owner's call):

   ```sh
   git push origin master
   git push origin v0.2.0
   ```

5. Watch the `release` workflow under the repository's
   [Actions tab](https://github.com/MonkeySaint/mailcraft/actions). When it succeeds, the release appears
   under [Releases](https://github.com/MonkeySaint/mailcraft/releases) with the notes generated from the
   commit subjects since the previous tag. Edit the notes there if needed. Re-running a failed `publish`
   job replaces the files of a release that already exists.

A version with a pre-release suffix (`0.2.0-rc.1`, tag `v0.2.0-rc.1`) is published as a pre-release. The
MSI cannot carry the suffix, so it installs as `0.2.0`.

To try the whole pipeline without publishing, run the `release` workflow by hand (Actions → release →
Run workflow). It builds every package and keeps them as run artifacts; the `publish` job only runs for
tags.

## What each job produces

| Job | Runner | Files |
|---|---|---|
| `version-check` | `ubuntu-latest` (`rust:1.99-bookworm` container) | (fails when the tag and `Cargo.toml` disagree) |
| `linux` (x86_64, aarch64) | `ubuntu-latest`, `ubuntu-24.04-arm` (`rust:1.99-bookworm` container) | `mailcraft-VERSION-linux-ARCH.tar.gz`, `mailcraft_VERSION-1_ARCH.deb`, `mailcraft-VERSION-1.ARCH.rpm`, `MailCraft-ARCH.AppImage` + `.zsync` |
| `appimage-smoke` (x86_64, aarch64) | `ubuntu-latest`, `ubuntu-24.04-arm` (offline `ubuntu:22.04` container) | (fails when the AppImage does not open its window, see [AppImage libraries](#appimage-libraries-and-minimum-system)) |
| `flatpak` | `ubuntu-latest` (privileged `debian:trixie` container) | `mailcraft-VERSION-x86_64.flatpak` |
| `web` | `ubuntu-latest` (`rust:1.99-bookworm` container) | `mailcraft-VERSION-web.zip` |
| `windows` | `windows-latest` | `mailcraft-VERSION-windows-x86_64.msi`, `mailcraft-VERSION-windows-x86_64.zip` |
| `macos` | `macos-latest` | `MailCraft-VERSION-macos-universal.dmg` |
| `freebsd` | `ubuntu-latest` (FreeBSD 14.3 VM, `vmactions/freebsd-vm`) | `mailcraft-VERSION-freebsd-ARCH.tar.gz`, `mailcraft-VERSION.pkg` |
| `publish` | `ubuntu-latest` | the GitHub release, all of the above, `SHA256SUMS` |

Every job runs by default. To leave a platform out, set the repository variable `RELEASE_WINDOWS`,
`RELEASE_MACOS` or `RELEASE_FREEBSD` to `false` (Settings → Secrets and variables → Actions → Variables).
Skipped platforms do not stop the release; failed ones do. The Rust version is the `rust:1.99-bookworm`
image on Linux and `RUST_TOOLCHAIN` (installed with rustup) on Windows and macOS; change them together.
FreeBSD uses the `rust` package of the VM's FreeBSD release.

Two more workflows build single platforms on demand (Actions → Run workflow): `freebsd.yml` (tests and
packages in the FreeBSD VM) and `windows-arm64.yml` (Windows on Arm, cross-compiled on `windows-latest` or
built natively on `windows-11-arm`). `packaging-lint.yml` runs ShellCheck, actionlint and
`packaging/check.sh` whenever `packaging/` or `.github/` changes. `ci.yml` runs `cargo xtask ci` (including
the wasm build) on every push to `master` and every pull request.

### Package contents

Every package holds `mailcraft` (the desktop app) and `mailcraft-cli`, the licences (`LICENSE-MIT`,
`LICENSE-APACHE`, `NOTICE`, `ATTRIBUTION.md` and the OFL texts of the fonts compiled into the binary) and,
on Linux and FreeBSD, the desktop file, AppStream metadata and hicolor icons named after the application
id `saint.dev.MailCraft`.

- **tar.gz** (Linux, FreeBSD): `bin/` and `share/` under one folder, ready to unpack into a prefix:
  `tar -xzf mailcraft-….tar.gz -C ~/.local --strip-components=1`.
- **deb / rpm** (built with [nfpm](https://nfpm.goreleaser.com), MIT): install into `/usr`; the post-install
  hook refreshes the desktop and icon caches.
- **AppImage**: carries the update information
  `gh-releases-zsync|MonkeySaint|mailcraft|latest|MailCraft-*ARCH.AppImage.zsync`, so AppImageUpdate
  fetches the matching `.zsync` from the newest GitHub release; this is why the AppImage file name has no
  version in it.
  It carries the windowing libraries the app opens at run time, see below.
- **Flatpak**: a single-file bundle on the Freedesktop 25.08 runtime from Flathub, with network, Wayland
  (X11 fallback), GPU and portal file dialogs: `flatpak install --user mailcraft-….flatpak`.
- **MSI**: per-machine install to `Program Files\MailCraft`, Start menu shortcut, `mailcraft-cli` on the
  system `PATH`. Upgrades replace older versions (fixed UpgradeCode in `packaging/windows/mailcraft.wxs`).
- **DMG**: `MailCraft.app`, a universal binary (Apple silicon and Intel), macOS 11 or later.
- **web zip**: static files to serve from any web server, with a `_headers` file (content security policy,
  `application/wasm`) for hosts that read one.

### AppImage libraries and minimum system

The app links only glibc and `libgcc_s`; winit, xkbcommon-dl, wayland-sys and glutin open the windowing
libraries with `dlopen` when the window is created. A desktop without one of them cannot open the window (base Ubuntu
22.04 has neither `libXcursor.so.1` nor `libxkbcommon-x11.so.0`, GitHub issue #23), so
`packaging/linux/appimage-libs.sh` (called by `appimage.sh`) copies them from the build system into
`usr/lib` of the AppDir and gives `usr/bin/mailcraft` the `DT_RPATH` `$ORIGIN/../lib` (with `patchelf`):

| Bundled | Why |
|---|---|
| `libXcursor.so.1`, `libXi.so.6` | opened by winit on X11 |
| `libXrender.so.1`, `libXfixes.so.3`, `libXext.so.6` | needed by libXcursor and libXi |
| `libxkbcommon.so.0`, `libxkbcommon-x11.so.0` | keyboard handling (X11 and Wayland) |
| `libxcb-xkb.so.1` | needed by libxkbcommon-x11 |
| `libwayland-cursor.so.0`, `libwayland-egl.so.1` | opened on Wayland |

Libraries on the [AppImage excludelist](https://github.com/AppImage/pkg2appimage/blob/master/excludelist)
are never bundled, because they have to match the system's drivers and display server: glibc, `libGL`,
`libEGL`, `libX11`, `libX11-xcb`, `libxcb`, `libwayland-client` and the rest of that list. The script
follows the `NEEDED` entries of everything it copies, fails when a library is missing on the build system
(`packaging/linux/ci-deps.sh` installs them in the release job) and copies the Debian copyright file of
each library's package to `usr/share/doc/mailcraft/third-party/` (all of them are MIT/X11-style licences).
`packaging/linux/appimage-libs.sh --check <AppDir>` reruns the checks on a filled AppDir.

The executable's `DT_RPATH` covers `dlopen` and the dependencies of every library loaded into the app,
and, unlike `LD_LIBRARY_PATH`, it does not reach the programs the app starts: the browser that opens a
link or the viewer that opens an attachment keeps the system's own (possibly newer) libraries. `AppRun`
therefore leaves the environment alone; the library checks and the smoke test below both fail when it
sets `LD_LIBRARY_PATH`.

The AppImage needs **glibc 2.35 or newer** (Ubuntu 22.04, Debian 12, Fedora 36 and later). It is built in
the `rust:1.99-bookworm` container (glibc 2.36), but nothing in it references a glibc symbol newer than
2.35; `appimage-libs.sh` fails the build when something does (`MC_APPIMAGE_MAX_GLIBC`, default `2.35`).
If that check ever fails, build the Linux packages on an older base instead of raising the limit.

The `appimage-smoke` job proves it on every release: `packaging/linux/appimage-test.sh` builds a plain
Ubuntu 22.04 image with only Xvfb, Mesa and xdotool (`packaging/linux/appimage-test/Containerfile`), runs
the AppImage in it with `--network none`, a new empty home and no arguments (as the AppImage catalog
does). It fails unless the MailCraft window appears within 30 s (`--timeout`), the app is still running
10 s after that (`--settle`), its environment has no `LD_LIBRARY_PATH`, and every library it must bundle
was loaded from the AppImage's `usr/lib`. The last check reads the running app's `/proc/PID/maps`: the
image lacks libXcursor, libXi, libXrender, libxkbcommon-x11 and libxcb-xkb, so a missing copy of one of
those stops the app anyway, but Mesa and xdotool install libXext, libXfixes and libxkbcommon, and a system
copy of those would otherwise hide a broken bundle or RPATH. `--self-test` (used by the release job) also
runs two broken variants of the AppImage, one that exits a second after its window appears and one that
loads libXext from the system, and fails unless the smoke test rejects both. The same works locally with
podman or docker:

```sh
packaging/linux/appimage-test.sh --self-test dist/MailCraft-x86_64.AppImage
```

Build that AppImage in the release job's container (`rust:1.99-bookworm`), not on a newer distribution:
an AppImage built on the developer machine takes that machine's glibc and libraries along, and the glibc
check stops it.

## Actions and downloaded tools

All jobs run on GitHub-hosted runners; there are no runners to register. Third-party actions are pinned to
released tags: `actions/checkout@v4`, `actions/cache@v4`, `actions/upload-artifact@v4`,
`actions/download-artifact@v4` and `vmactions/freebsd-vm@v1`. The `publish` job uses the `gh` CLI that comes
with the runner and the job's `GITHUB_TOKEN` (`permissions: contents: write`); no personal token is needed.
JavaScript actions in container jobs use the Node.js that the runner mounts into the container.

Some build tools are downloaded from their upstream release pages during the jobs (none of them ends up in
a package): nfpm (checked against its release checksums), appimagetool and the AppImage type2 runtime
(`continuous` releases), Trunk, ShellCheck and actionlint (checked against pinned SHA-256 sums). They are
cached with `actions/cache` under `~/.cache/mailcraft-packaging` and `~/.cache/mailcraft-lint`. The
`appimage-smoke` job builds its test image from `ubuntu:22.04` (Docker Hub) and the Ubuntu archive.

## Secrets and variables

Settings → Secrets and variables → Actions of the repository. All of them are optional: without the
signing secrets the Windows and macOS packages are built unsigned (ad-hoc signed on macOS).

| Name | Kind | Used for |
|---|---|---|
| `WINDOWS_CERT_PFX` | secret | Base64 of the code-signing certificate (`.pfx`). With `WINDOWS_CERT_PASSWORD`, the `.exe` files and the MSI are signed with `signtool` and timestamped; without them they are unsigned. |
| `WINDOWS_CERT_PASSWORD` | secret | Password of that `.pfx`. |
| `APPLE_CERT_P12` | secret | Base64 of a "Developer ID Application" certificate and key (`.p12`). |
| `APPLE_CERT_PASSWORD` | secret | Its password. |
| `APPLE_SIGNING_IDENTITY` | secret | e.g. `Developer ID Application: Name (TEAMID)`. With the two above: hardened-runtime signing of the app and DMG. Without: ad-hoc signature. |
| `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` | secrets | Notarisation with `xcrun notarytool` (app-specific password) and stapling, after signing. |
| `RELEASE_WINDOWS`, `RELEASE_MACOS`, `RELEASE_FREEBSD` | variables | `false` skips that platform's job. |

The deb/rpm maintainer field defaults to `MonkeySaint <monkey@saint.dev>`; set `MAILCRAFT_MAINTAINER` in
the environment to change it.

## Building packages locally

Every job is a script under `packaging/` that also runs on a developer machine; output goes to `dist/`
(gitignored) unless `--out` says otherwise. Settings shared by all of them, including the application id,
live in `packaging/env.sh`.

```sh
# Linux: build, then tar.gz + deb + rpm (deb/rpm need nfpm on PATH or NFPM=/path/to/nfpm)
packaging/linux/package.sh --build
# the same from a debug build, to try the scripts quickly
cargo build -p mailcraft -p mailcraft-cli && packaging/linux/package.sh --profile debug
# AppImage from the built binaries (appimagetool on PATH, or MC_DOWNLOAD_TOOLS=1 to fetch it)
packaging/linux/appimage.sh
# start the AppImage offline in a plain Ubuntu 22.04 container (podman or docker)
packaging/linux/appimage-test.sh dist/MailCraft-x86_64.AppImage
# Flatpak bundle (flatpak-builder installed)
packaging/linux/flatpak/flatpak-bundle.sh --bin-dir target/release
# web zip (Trunk if installed, else cargo + wasm-bindgen-cli)
packaging/web/package.sh
# macOS (on a Mac), Windows (PowerShell), FreeBSD (on FreeBSD)
packaging/macos/package.sh
pwsh packaging/windows/package.ps1
bash packaging/freebsd/package.sh --build
# consistency checks and linters
packaging/check.sh
shellcheck packaging/*.sh packaging/*/*.sh packaging/*/*/*.sh
actionlint -config-file .github/actionlint.yaml .github/workflows/*.yml
```

### Running a workflow job locally

[act](https://github.com/nektos/act) runs a workflow from the working copy in local containers (Docker or
Podman needed). It suits the container jobs on Linux:

```sh
act workflow_dispatch -W .github/workflows/release.yml -j linux
act workflow_dispatch -W .github/workflows/packaging-lint.yml -j lint
```

The Windows, macOS and FreeBSD jobs and `publish` need GitHub's runners (and, for `publish`, a tag), so
they cannot run this way; their scripts run directly on a machine of that platform (above).

## Version and build details

`mailcraft --version` prints `mailcraft X.Y.Z (git <hash>, <date>)`; the Account page of the File tab shows
the same after "Version". The hash and date come from `MAILCRAFT_GIT_HASH` and `MAILCRAFT_BUILD_DATE`,
which `packaging/env.sh` (and `package.ps1` on Windows) exports for release builds: the short commit hash
and the commit date, so rebuilding a tag gives the same text. A plain `cargo build` of the desktop app
gets them from `git` in `apps/mailcraft/build.rs`. `mailcraft-cli --version` adds them only when they were
in the build environment.
