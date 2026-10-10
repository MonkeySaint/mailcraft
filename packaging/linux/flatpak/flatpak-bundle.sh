#!/usr/bin/env bash
# Single-file Flatpak bundle from already-built Linux binaries.
#
#     packaging/linux/flatpak/flatpak-bundle.sh (--bin-dir DIR | --tarball FILE) [--arch ARCH] [--out DIR]
#
#   --bin-dir  folder with mailcraft and mailcraft-cli (e.g. target/release)
#   --tarball  the mailcraft-VERSION-linux-ARCH.tar.gz from package.sh (what the release job passes)
#   --arch     x86_64 (default: host) or aarch64; must match the binaries
#
# Output: mailcraft-VERSION-ARCH.flatpak; install with `flatpak install --user mailcraft-….flatpak`
# (the Freedesktop runtime comes from Flathub, recorded in the bundle).
#
# Needs flatpak and flatpak-builder. The Freedesktop runtime and SDK are installed per user from Flathub on
# first use. flatpak-builder sandboxes with bubblewrap, so inside a container it needs a privileged one.
set -euo pipefail
here="$(dirname "${BASH_SOURCE[0]}")"
# shellcheck source=packaging/env.sh
. "$here/../../env.sh"

bin_dir=""
tarball=""
arch="$(uname -m)"
out="$MC_DIST"
while [ $# -gt 0 ]; do
	case "$1" in
	--bin-dir) bin_dir="${2:?--bin-dir needs a value}"; shift ;;
	--tarball) tarball="${2:?--tarball needs a value}"; shift ;;
	--arch) arch="${2:?--arch needs a value}"; shift ;;
	--out) out="${2:?--out needs a value}"; shift ;;
	-h | --help) sed -n '2,15p' "$0"; exit 0 ;;
	*) mc_die "unknown option $1" ;;
	esac
	shift
done
[ -n "$bin_dir" ] || [ -n "$tarball" ] || mc_die "pass --bin-dir or --tarball"
case "$arch" in
amd64) arch="x86_64" ;;
arm64) arch="aarch64" ;;
esac

manifest="$here/$APP_ID.yml"
[ -f "$manifest" ] || mc_die "missing $manifest (its name must match APP_ID in packaging/env.sh)"
grep -qx "id: $APP_ID" "$manifest" || mc_die "$manifest: id is not $APP_ID"
command -v flatpak-builder >/dev/null 2>&1 || mc_die "flatpak-builder not found"

work="${CARGO_TARGET_DIR:-$MC_ROOT/target}/packaging/flatpak-$arch"
rm -rf "$work"
mkdir -p "$work/stage" "$out"
out="$(cd "$out" && pwd)"

if [ -n "$tarball" ]; then
	tar -xzf "$tarball" -C "$work/stage" --strip-components=1
else
	"$MC_ROOT/packaging/stage-unix.sh" "$bin_dir" "$work/stage"
fi
[ -x "$work/stage/bin/mailcraft" ] || mc_die "no bin/mailcraft in the staged files"
cp "$manifest" "$work/$APP_ID.yml"

flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
(cd "$work" && flatpak-builder --user --arch="$arch" --install-deps-from=flathub --force-clean \
	--disable-rofiles-fuse --repo=repo build "$APP_ID.yml")
bundle="$out/mailcraft-$MC_VERSION-$arch.flatpak"
flatpak build-bundle --arch="$arch" --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
	"$work/repo" "$bundle" "$APP_ID"
mc_log "wrote $bundle"
