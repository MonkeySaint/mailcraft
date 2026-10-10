#!/usr/bin/env bash
# FreeBSD packages, built natively on a FreeBSD machine (self-hosted `freebsd` runner).
#
#     packaging/freebsd/package.sh [--build] [--profile release|debug] [--out DIR]
#
# Needs: bash, git, rust (pkg install rust, or rustup), and for the desktop app at run time the Wayland or
# X11 and Mesa libraries (pkg install libxkbcommon wayland mesa-libs).
#
# Output (ARCH from `uname -m`, e.g. amd64 or arm64; VERSION from Cargo.toml):
#   mailcraft-VERSION-freebsd-ARCH.tar.gz   bin/ and share/ laid out for /usr/local
#   mailcraft-VERSION.pkg                   with `pkg` available: install with `pkg add ./mailcraft-….pkg`
#                                           (it records no dependencies; install the libraries above)
set -euo pipefail
# shellcheck source=packaging/env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

build=0
profile="release"
out="$MC_DIST"
while [ $# -gt 0 ]; do
	case "$1" in
	--build) build=1 ;;
	--profile) profile="${2:?--profile needs a value}"; shift ;;
	--out) out="${2:?--out needs a value}"; shift ;;
	-h | --help) sed -n '2,12p' "$0"; exit 0 ;;
	*) mc_die "unknown option $1" ;;
	esac
	shift
done

arch="$(uname -m)"
if [ "$build" = 1 ]; then
	cargo_profile="$profile"
	[ "$profile" = "debug" ] && cargo_profile="dev"
	mc_log "cargo build --profile $cargo_profile (git $MAILCRAFT_GIT_HASH, $MAILCRAFT_BUILD_DATE)"
	(cd "$MC_ROOT" && cargo build --locked --profile "$cargo_profile" -p mailcraft -p mailcraft-cli)
fi
bin_dir="$(mc_bin_dir "" "$profile")"

work="${CARGO_TARGET_DIR:-$MC_ROOT/target}/packaging/freebsd-$arch"
rm -rf "$work"
mkdir -p "$work" "$out"
out="$(cd "$out" && pwd)"

name="mailcraft-$MC_VERSION-freebsd-$arch"
"$MC_ROOT/packaging/stage-unix.sh" "$bin_dir" "$work/$name"
mc_tar_gz "$out/$name.tar.gz" "$work/$name"
mc_log "wrote $out/$name.tar.gz"

if command -v pkg >/dev/null 2>&1; then
	# pkg-create(8) from a staged root: +MANIFEST (UCL) plus a plist of files relative to the prefix.
	root="$work/pkgroot"
	"$MC_ROOT/packaging/stage-unix.sh" "$bin_dir" "$root/usr/local"
	(cd "$root/usr/local" && find . -type f | sed 's|^\./||' | sort) >"$work/plist"
	cat >"$work/+MANIFEST" <<MANIFEST
name: mailcraft
version: "$MC_VERSION"
origin: mail/mailcraft
comment: "$APP_SUMMARY"
desc: "MailCraft is an open-source desktop mail client written in Rust, with calendar, people, tasks and notes. Includes mailcraft-cli."
maintainer: "$MAINTAINER"
www: "$HOMEPAGE"
prefix: /usr/local
licenselogic: or
licenses: [MIT, APACHE20]
categories: [mail]
MANIFEST
	pkg create -M "$work/+MANIFEST" -p "$work/plist" -r "$root" -o "$out"
	mc_log "wrote $out/mailcraft-$MC_VERSION.pkg"
else
	mc_log "pkg not found (not on FreeBSD?): skipping the .pkg"
fi
