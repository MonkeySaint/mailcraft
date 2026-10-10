# shellcheck shell=bash
# Shared settings for every packaging script. Source it, do not run it:
#
#     . "$(dirname "$0")/../env.sh"
#
# This file is the single place for the application id, names and the forge URLs. Files that cannot read
# shell variables (the .desktop file name, the Flatpak manifest, the WiX source) carry the same values, and
# `packaging/check.sh` fails when they drift apart.

# The constants are read by the scripts that source this file.
# shellcheck disable=SC2034

# Reverse-DNS application id: desktop file, AppStream id, icon name, Flatpak id, macOS bundle id.
APP_ID="saint.dev.MailCraft"
APP_NAME="MailCraft"
APP_SUMMARY="Desktop mail, calendar and contacts in the classic three-pane layout"
# Executables shipped in every package.
APP_BINS="mailcraft mailcraft-cli"

FORGE_URL="https://github.com"
FORGE_REPO="MonkeySaint/mailcraft"
HOMEPAGE="$FORGE_URL/$FORGE_REPO"
# Files on the default branch (AppStream screenshots).
RAW_URL="$HOMEPAGE/raw/branch/main"
# GitHub serves the newest release's asset at releases/download/latest/<file> (when no tag is literally
# named `latest`); AppImage update information points there.
LATEST_DOWNLOAD_URL="$HOMEPAGE/releases/download/latest"
# deb/rpm Maintainer/Packager (Debian policy wants a name and an address).
MAINTAINER="${MAILCRAFT_MAINTAINER:-MonkeySaint <monkey@saint.dev>}"
VENDOR="MailCraft contributors"
LICENSE_SPDX="MIT OR Apache-2.0"

# Repository root, whatever the caller's working directory.
MC_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Output folder for finished packages (gitignored `/dist`); scripts take `--out` to override.
MC_DIST="${MC_DIST:-$MC_ROOT/dist}"

mc_log() { printf '==> %s\n' "$*" >&2; }
mc_die() {
	printf 'error: %s\n' "$*" >&2
	exit 1
}

# `version = "X.Y.Z"` from [workspace.package] of the root Cargo.toml (what `cargo xtask version show`
# prints), without building anything.
mc_version() {
	awk '
		/^\[/ { in_section = ($0 == "[workspace.package]"); next }
		in_section && $1 == "version" {
			sub(/^[^"]*"/, ""); sub(/".*$/, ""); print; exit
		}
	' "$MC_ROOT/Cargo.toml"
}

MC_VERSION="$(mc_version)"
[ -n "$MC_VERSION" ] || mc_die "no version in [workspace.package] of $MC_ROOT/Cargo.toml"

# Build details for `--version` and the About page (read by apps/mailcraft/build.rs and, through
# option_env!, by mailcraft-cli and the UI). The date is the commit date so rebuilding a tag gives the
# same output; SOURCE_DATE_EPOCH also pins timestamps inside the archives.
if [ -z "${MAILCRAFT_GIT_HASH:-}" ]; then
	MAILCRAFT_GIT_HASH="$(git -C "$MC_ROOT" rev-parse --short=10 HEAD 2>/dev/null || echo unknown)"
fi
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
	SOURCE_DATE_EPOCH="$(git -C "$MC_ROOT" log -1 --format=%ct 2>/dev/null || date +%s)"
fi
if [ -z "${MAILCRAFT_BUILD_DATE:-}" ]; then
	# GNU date takes -d @N, BSD date (macOS, FreeBSD) takes -r N.
	MAILCRAFT_BUILD_DATE="$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y-%m-%d 2>/dev/null ||
		date -u -r "$SOURCE_DATE_EPOCH" +%Y-%m-%d 2>/dev/null || echo unknown)"
fi
export MAILCRAFT_GIT_HASH MAILCRAFT_BUILD_DATE SOURCE_DATE_EPOCH

# SHA-256 of a file, hex only (sha256sum on Linux, shasum on macOS, sha256 on FreeBSD).
mc_sha256() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$1" | cut -d' ' -f1
	else
		sha256 -q "$1"
	fi
}

# Folder holding the built binaries for a target triple and profile, honouring CARGO_TARGET_DIR.
# Without a triple, the host build folder (target/<profile>).
mc_bin_dir() {
	local triple="$1" profile="$2" base="${CARGO_TARGET_DIR:-$MC_ROOT/target}"
	local sub="$profile"
	[ "$profile" = "dev" ] && sub="debug"
	if [ -n "$triple" ]; then
		echo "$base/$triple/$sub"
	else
		echo "$base/$sub"
	fi
}

# Reproducible archive of folder $2 (its own name as the top level) at $1 (.tar.gz).
mc_tar_gz() {
	local out="$1" dir="$2"
	local parent base
	parent="$(dirname "$dir")"
	base="$(basename "$dir")"
	if tar --version 2>/dev/null | grep -q 'GNU tar'; then
		tar -C "$parent" --sort=name --owner=0 --group=0 --numeric-owner \
			--mtime="@$SOURCE_DATE_EPOCH" -cf - "$base" | gzip -9n >"$out"
	else
		tar -C "$parent" -cf - "$base" | gzip -9n >"$out"
	fi
}
