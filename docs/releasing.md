# Releasing MailCraft

Releases are built by GitHub Actions on github.com (`.github/workflows/release.yml`) and published as a
GitHub release with every package and a `SHA256SUMS` file.

> **Status:** no Actions runners are registered on github.com yet, so neither CI nor releases run.
> Until the owner registers at least a `docker` runner (below), pushing a tag does nothing but sit in the
> queue. Everything in this document can still be run by hand on a developer machine.

## Cutting a release

1. Make sure `main` is green: `cargo xtask ci`.
2. Bump the version (root `Cargo.toml` `[workspace.package]` and the workspace entries in `Cargo.lock`):

   ```sh
   cargo xtask version bump 0.2.0
   ```

3. Commit and tag. The tag must be `v` + the workspace version; the `version-check` job fails otherwise.

   ```sh
   git commit -am "Release 0.2.0"
   git tag -a v0.2.0 -m "MailCraft 0.2.0"
   ```

4. Push the commit, then the tag (pushing is the owner's call):

   ```sh
   git push origin main
   git push origin v0.2.0
   ```

5. Watch the `release` workflow under the repository's Actions tab. When it succeeds, the release appears
   under Releases with the notes generated from the commit subjects since the previous tag. Edit the
   notes there if needed.

A version with a pre-release suffix (`0.2.0-rc.1`, tag `v0.2.0-rc.1`) is published as a pre-release. The
MSI cannot carry the suffix, so it installs as `0.2.0`.

To try the whole pipeline without publishing, run the `release` workflow by hand (Actions → release →
Run workflow). It builds every package and keeps them as run artifacts; the `publish` job only runs for
tags.

## What each job produces

| Job | Runner label | Files |
|---|---|---|
| `version-check` | `docker` | (fails when the tag and `Cargo.toml` disagree) |
| `linux` (x86_64, aarch64) | `docker` | `mailcraft-VERSION-linux-ARCH.tar.gz`, `mailcraft_VERSION-1_ARCH.deb`, `mailcraft-VERSION-1.ARCH.rpm`, `MailCraft-ARCH.AppImage` + `.zsync` |
| `flatpak` | `docker` (privileged) | `mailcraft-VERSION-x86_64.flatpak` |
| `web` | `docker` | `mailcraft-VERSION-web.zip` |
| `windows` | `windows` | `mailcraft-VERSION-windows-x86_64.msi`, `mailcraft-VERSION-windows-x86_64.zip` |
| `macos` | `macos` | `MailCraft-VERSION-macos-universal.dmg` |
| `freebsd` | `freebsd` | `mailcraft-VERSION-freebsd-ARCH.tar.gz`, `mailcraft-VERSION.pkg` |
| `publish` | `docker` | the GitHub release, all of the above, `SHA256SUMS` |

The `windows`, `macos` and `freebsd` jobs only run when the repository variables `RELEASE_WINDOWS`,
`RELEASE_MACOS` and `RELEASE_FREEBSD` are `true` (Settings → Actions → Variables). A job whose label has
no runner waits forever and would hold the release back, so turn a platform on only once its runner is
registered. Skipped platforms do not stop the release; failed ones do.

Two more workflows build single platforms on demand (Actions → Run workflow): `freebsd.yml` and
`windows-arm64.yml` (Windows on Arm, cross-compiled on the `windows` runner or native on a
`windows-arm64` one). `packaging-lint.yml` runs ShellCheck, actionlint and `packaging/check.sh` whenever
`packaging/` or `.github/` changes.

### Package contents

Every package holds `mailcraft` (the desktop app) and `mailcraft-cli`, the licences (`LICENSE-MIT`,
`LICENSE-APACHE`, `NOTICE`, `ATTRIBUTION.md` and the OFL texts of the fonts compiled into the binary) and,
on Linux and FreeBSD, the desktop file, AppStream metadata and hicolor icons named after the application
id `saint.dev.MailCraft`.

- **tar.gz** (Linux, FreeBSD): `bin/` and `share/` under one folder, ready to unpack into a prefix:
  `tar -xzf mailcraft-….tar.gz -C ~/.local --strip-components=1`.
- **deb / rpm** (built with [nfpm](https://nfpm.goreleaser.com), MIT): install into `/usr`; the post-install
  hook refreshes the desktop and icon caches.
- **AppImage**: carries update information
  `zsync|https://github.com/MonkeySaint/mailcraft/releases/download/latest/MailCraft-ARCH.AppImage.zsync`.
  GitHub serves the newest release's file at `releases/download/latest/…`, so AppImageUpdate finds new
  versions; this is why the AppImage file name has no version in it. Never create a tag literally named
  `latest`: it would shadow that alias.
- **Flatpak**: a single-file bundle on the Freedesktop 25.08 runtime from Flathub, with network, Wayland
  (X11 fallback), GPU and portal file dialogs: `flatpak install --user mailcraft-….flatpak`.
- **MSI**: per-machine install to `Program Files\MailCraft`, Start menu shortcut, `mailcraft-cli` on the
  system `PATH`. Upgrades replace older versions (fixed UpgradeCode in `packaging/windows/mailcraft.wxs`).
- **DMG**: `MailCraft.app`, a universal binary (Apple silicon and Intel), macOS 11 or later.
- **web zip**: static files to serve from any web server, with a `_headers` file (content security policy,
  `application/wasm`) for hosts that read one.

## Runners

Runners are registered by the owner on github.com (Site or repository settings → Actions → Runners) with
[github-runner](https://github.org/docs/latest/admin/actions/runner-installation/). Labels the workflows use:

| Label | Kind | Needed for | Notes |
|---|---|---|---|
| `docker` | Docker or Podman host (x86_64 Linux) | CI, release (required) | Jobs run in `rust:1.99-bookworm`, `node:22-bookworm` and `debian:bookworm` containers. The `flatpak` job needs privileged containers: `container.privileged: true` in the runner config. |
| `docker-arm64` | Docker host on aarch64 Linux | optional | Set the repository variable `LINUX_ARM64_LABEL=docker-arm64` to build aarch64 Linux natively; otherwise it is cross-compiled on `docker` with `gcc-aarch64-linux-gnu`. |
| `windows` | Windows x64 host (runner in host mode) | `RELEASE_WINDOWS` | Rust (rustup, MSVC toolchain), Visual Studio Build Tools, git, PowerShell 7, Node.js (for the JavaScript actions), .NET SDK (WiX v4 is installed as a dotnet tool on first use). For `windows-arm64.yml`: the MSVC ARM64 build tools and LLVM/clang. |
| `macos` | Mac (runner in host mode) | `RELEASE_MACOS` | Xcode command-line tools, rustup, git, Node.js. |
| `freebsd` | FreeBSD machine or VM (runner in host mode) | `RELEASE_FREEBSD` | `pkg install bash git rust node`. The desktop app needs `libxkbcommon wayland mesa-libs` at run time. |

GitHub's hosted runners and `github.com` actions are not used. `actions/checkout`, `actions/cache`,
`actions/upload-artifact` and `actions/download-artifact` are resolved by GitHub from its default action
mirror; `github-release` comes from `https://code.github.org/actions/github-release`. The artifact
actions at `@v4` need GitHub 7 or later. Jobs in the `rust:1.99-bookworm` image install `nodejs` first,
because JavaScript actions run inside the job container.

Some build tools are downloaded from their upstream release pages during the jobs (none of them ends up in
a package): nfpm (checked against its release checksums), appimagetool and the AppImage type2 runtime
(`continuous` releases), Trunk, ShellCheck and actionlint (checked against pinned SHA-256 sums). They are
cached with `actions/cache` under `~/.cache/mailcraft-packaging` and `~/.cache/mailcraft-lint`.

## Secrets and variables

Settings → Actions → Secrets / Variables of the repository.

| Name | Kind | Required | Used for |
|---|---|---|---|
| `GITHUB_TOKEN` | secret | recommended | Application token with `write:repository` for publishing the release. Without it the job token (`github.token`) is used, which works only when it may write releases. |
| `WINDOWS_CERT_PFX` | secret | optional | Base64 of the code-signing certificate (`.pfx`). With `WINDOWS_CERT_PASSWORD`, the `.exe` files and the MSI are signed with `signtool` and timestamped; without them they are unsigned. |
| `WINDOWS_CERT_PASSWORD` | secret | optional | Password of that `.pfx`. |
| `APPLE_CERT_P12` | secret | optional | Base64 of a "Developer ID Application" certificate and key (`.p12`). |
| `APPLE_CERT_PASSWORD` | secret | optional | Its password. |
| `APPLE_SIGNING_IDENTITY` | secret | optional | e.g. `Developer ID Application: Name (TEAMID)`. With the two above: hardened-runtime signing of the app and DMG. Without: ad-hoc signature. |
| `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` | secrets | optional | Notarisation with `xcrun notarytool` (app-specific password) and stapling, after signing. |
| `RELEASE_WINDOWS`, `RELEASE_MACOS`, `RELEASE_FREEBSD` | variables | optional | `true` turns that platform's job on. |
| `LINUX_ARM64_LABEL` | variable | optional | Runner label for native aarch64 Linux builds (e.g. `docker-arm64`). |

The deb/rpm maintainer field defaults to `MonkeySaint <monkey@saint.dev>`; set
`MAILCRAFT_MAINTAINER` in the environment to change it.

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

### Running a workflow job with github-runner

`github-runner exec` runs a workflow from the working copy in local containers, without a GitHub server
(Docker or Podman needed; JavaScript actions are fetched from the default action mirror):

```sh
github-runner exec -W .github/workflows/release.yml -j linux --event workflow_dispatch
github-runner exec -W .github/workflows/packaging-lint.yml -j lint
```

Jobs that set `container:` run in that image. For jobs on host labels (`windows`, `macos`, `freebsd`),
`--image -self-hosted` runs them directly on the current machine. Secrets go in with `-s NAME=value`. The
exact flags vary between runner versions: check `github-runner exec --help`. The `publish` job needs a
real GitHub instance and a tag, so it cannot run this way.

## Version and build details

`mailcraft --version` prints `mailcraft X.Y.Z (git <hash>, <date>)`; the Account page of the File tab shows
the same after "Version". The hash and date come from `MAILCRAFT_GIT_HASH` and `MAILCRAFT_BUILD_DATE`,
which `packaging/env.sh` (and `package.ps1` on Windows) exports for release builds: the short commit hash
and the commit date, so rebuilding a tag gives the same text. A plain `cargo build` of the desktop app
gets them from `git` in `apps/mailcraft/build.rs`. `mailcraft-cli --version` adds them only when they were
in the build environment.
