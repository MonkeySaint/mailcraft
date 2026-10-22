#!/usr/bin/env bash
# AppImage of the desktop app (mailcraft-cli rides along in usr/bin).
#
#     packaging/linux/appimage.sh [--target TRIPLE] [--profile release|debug] [--out DIR] [--no-update-info]
#
# Expects the binaries to be built already (packaging/linux/package.sh --build does that).
# Output: MailCraft-ARCH.AppImage and MailCraft-ARCH.AppImage.zsync. The name carries no version on purpose:
# the update information embedded in the AppImage is
#     gh-releases-zsync|MonkeySaint|mailcraft|latest|MailCraft-*ARCH.AppImage.zsync
# and AppImageUpdate fetches the matching file from the newest GitHub release.
#
# The windowing libraries the app opens at run time come from the build system:
# packaging/linux/appimage-libs.sh copies them into usr/lib (all but the AppImage excludelist), gives
# usr/bin/mailcraft the RPATH $ORIGIN/../lib and checks the AppDir (nothing missing, glibc 2.35 at most).
# Build the AppImage on Debian 12 / Ubuntu 22.04 or older (the release job uses rust:1.99-bookworm).
# packaging/linux/appimage-test.sh starts the result offline in a plain Ubuntu 22.04 container.
#
# Tools:
#   appimagetool  $APPIMAGETOOL, else PATH, else (MC_DOWNLOAD_TOOLS=1) downloaded once from
#                 https://github.com/AppImage/appimagetool/releases (continuous) into $MC_TOOL_CACHE
#                 (default ~/.cache/mailcraft-packaging; CI keeps that folder with actions/cache).
#                 It runs with APPIMAGE_EXTRACT_AND_RUN=1, so containers need no FUSE.
#   runtime       for a foreign architecture (aarch64 on an x86_64 host) the type2 runtime
#                 runtime-ARCH from https://github.com/AppImage/type2-runtime/releases (continuous), cached
#                 the same way, or $APPIMAGE_RUNTIME.
#   zsyncmake     (package `zsync`) writes the .zsync file; without it only the AppImage is produced.
#   readelf, patchelf  (packages `binutils`, `patchelf`) for appimage-libs.sh.
set -euo pipefail
# shellcheck source=packaging/env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

triple=""
profile="release"
out="$MC_DIST"
update_info=1
while [ $# -gt 0 ]; do
	case "$1" in
	--target) triple="${2:?--target needs a value}"; shift ;;
	--profile) profile="${2:?--profile needs a value}"; shift ;;
	--out) out="${2:?--out needs a value}"; shift ;;
	--no-update-info) update_info=0 ;;
	-h | --help) sed -n '2,27p' "$0"; exit 0 ;;
	*) mc_die "unknown option $1" ;;
	esac
	shift
done

arch="${triple%%-*}"
[ -n "$arch" ] || arch="$(uname -m)"
case "$arch" in
x86_64 | amd64) arch="x86_64" ;;
aarch64 | arm64) arch="aarch64" ;;
*) mc_die "unsupported architecture $arch" ;;
esac
host_arch="$(uname -m)"
[ "$host_arch" = "arm64" ] && host_arch="aarch64"

cache="${MC_TOOL_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/mailcraft-packaging}"

# Prints the path of a cached download, fetching it first when allowed.
fetch_tool() {
	local url="$1" file="$cache/$2"
	if [ ! -s "$file" ]; then
		[ "${MC_DOWNLOAD_TOOLS:-0}" = 1 ] || return 1
		mkdir -p "$cache"
		mc_log "downloading $url"
		curl -fsSL --retry 3 -o "$file.part" "$url"
		mv "$file.part" "$file"
		chmod +x "$file"
	fi
	echo "$file"
}

tool="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
if [ -z "$tool" ]; then
	tool="$(fetch_tool "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$host_arch.AppImage" "appimagetool-$host_arch.AppImage")" ||
		mc_die "appimagetool not found: install it, set APPIMAGETOOL, or set MC_DOWNLOAD_TOOLS=1"
fi

runtime="${APPIMAGE_RUNTIME:-}"
if [ -z "$runtime" ] && [ "$arch" != "$host_arch" ]; then
	runtime="$(fetch_tool "https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-$arch" "runtime-$arch")" ||
		mc_die "no AppImage runtime for $arch: set APPIMAGE_RUNTIME or MC_DOWNLOAD_TOOLS=1"
fi

bin_dir="$(mc_bin_dir "$triple" "$profile")"
work="${CARGO_TARGET_DIR:-$MC_ROOT/target}/packaging/appimage-$arch"
appdir="$work/MailCraft.AppDir"
rm -rf "$work"
mkdir -p "$appdir" "$out"
out="$(cd "$out" && pwd)"

"$MC_ROOT/packaging/stage-unix.sh" "$bin_dir" "$appdir/usr"
install -m 0644 "$appdir/usr/share/applications/$APP_ID.desktop" "$appdir/$APP_ID.desktop"
install -m 0644 "$appdir/usr/share/icons/hicolor/256x256/apps/$APP_ID.png" "$appdir/$APP_ID.png"
cp "$appdir/$APP_ID.png" "$appdir/.DirIcon"
# Older appimagetool builds look for the AppStream file under its legacy name.
cp "$appdir/usr/share/metainfo/$APP_ID.metainfo.xml" "$appdir/usr/share/metainfo/$APP_ID.appdata.xml"
cat >"$appdir/AppRun" <<'EOF'
#!/bin/sh
# AppImage entry point: start the desktop app from inside the mounted image. The executable's RPATH
# ($ORIGIN/../lib) finds the bundled windowing libraries in usr/lib (see packaging/linux/appimage-libs.sh);
# the environment stays as it is, so programs the app opens use the system's libraries.
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/mailcraft" "$@"
EOF
chmod 0755 "$appdir/AppRun"

"$MC_ROOT/packaging/linux/appimage-libs.sh" --arch "$arch" "$appdir"

image="$out/MailCraft-$arch.AppImage"
rm -f "$image" "$image.zsync"
# -n: appimagetool's AppStream check fetches every URL in the metainfo, which fails while the repository
# is private or offline; packaging/check.sh validates the same file with `appstreamcli validate --no-net`.
args=(-n)
[ -n "$runtime" ] && args+=(--runtime-file "$runtime")
update_info_str="gh-releases-zsync|$GH_OWNER|$GH_REPO|latest|MailCraft-*$arch.AppImage.zsync"
[ "$update_info" = 1 ] && args+=(-u "$update_info_str")
mc_log "appimagetool ${args[*]} → $image"
# SOURCE_DATE_EPOCH is dropped for this call: appimagetool passes its own timestamp options to mksquashfs,
# and mksquashfs 4.6+ refuses both together.
(cd "$work" && env -u SOURCE_DATE_EPOCH ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 "$tool" "${args[@]}" "$appdir" "$image")
[ -s "$image" ] || mc_die "appimagetool did not write $image"

# appimagetool writes the .zsync itself when zsyncmake is on PATH; it lands in the working folder with
# older builds, next to the image with newer ones.
if [ "$update_info" = 1 ] && [ ! -f "$image.zsync" ]; then
	if [ -f "$work/$(basename "$image").zsync" ]; then
		mv "$work/$(basename "$image").zsync" "$image.zsync"
	elif command -v zsyncmake >/dev/null 2>&1; then
		(cd "$out" && zsyncmake -u "$(basename "$image")" -o "$image.zsync" "$image")
	else
		mc_log "warning: zsyncmake not found; no $image.zsync (install the zsync package)"
	fi
fi
mc_log "wrote $image"
