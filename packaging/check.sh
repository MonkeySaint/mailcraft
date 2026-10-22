#!/usr/bin/env bash
# Consistency checks for the packaging files (no build needed); run by .github/workflows/packaging-lint.yml.
#
#     packaging/check.sh
#
# - files named after APP_ID exist and carry it (desktop file, Flatpak manifest)
# - the WiX source links to HOMEPAGE
# - the desktop file passes desktop-file-validate and the filled-in AppStream file passes
#   `appstreamcli validate --no-net` (each when the tool is installed)
# - XML templates are well-formed (xmllint, when installed)
# - every icon size the scripts expect is present
# - packaging/linux/appimage-libs.sh --check rejects an AppDir that lacks the run-time libraries or needs
#   a newer glibc than allowed (when readelf is installed)
# - packaging/linux/appimage-smoke.sh --check-maps rejects an app that loads a bundled library from the
#   system, or never loads one it must bundle
set -euo pipefail
# shellcheck source=packaging/env.sh
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

fail=0
bad() {
	printf 'check: %s\n' "$*" >&2
	fail=1
}
have() { command -v "$1" >/dev/null 2>&1; }

p="$MC_ROOT/packaging"
desktop="$p/linux/$APP_ID.desktop"
manifest="$p/linux/flatpak/$APP_ID.yml"
[ -f "$desktop" ] || bad "missing $desktop"
[ -f "$manifest" ] || bad "missing $manifest"
[ -f "$manifest" ] && { grep -qx "id: $APP_ID" "$manifest" || bad "$manifest: id is not $APP_ID"; }
[ -f "$desktop" ] && { grep -qx "Icon=$APP_ID" "$desktop" || bad "$desktop: Icon is not $APP_ID"; }
[ -f "$desktop" ] && { grep -qx "StartupWMClass=$APP_ID" "$desktop" || bad "$desktop: StartupWMClass is not $APP_ID"; }
grep -q "Value=\"$HOMEPAGE\"" "$p/windows/mailcraft.wxs" || bad "mailcraft.wxs: ARPURLINFOABOUT is not $HOMEPAGE"
grep -qx "license: $LICENSE_SPDX" "$p/linux/nfpm.yaml" || bad "nfpm.yaml: license is not $LICENSE_SPDX"
grep -qx "vendor: $VENDOR" "$p/linux/nfpm.yaml" || bad "nfpm.yaml: vendor is not $VENDOR"
grep -q '@APP_ID@' "$p/linux/metainfo.xml.in" || bad "metainfo.xml.in does not use @APP_ID@"
grep -q '@APP_ID@' "$p/macos/Info.plist.in" || bad "Info.plist.in does not use @APP_ID@"
for size in 16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512; do
	[ -f "$MC_ROOT/assets/app-icon/hicolor/$size/apps/mailcraft.png" ] || bad "missing $size icon"
done
[ -f "$MC_ROOT/assets/app-icon/hicolor/scalable/apps/mailcraft.svg" ] || bad "missing scalable icon"
[ -f "$MC_ROOT/assets/app-icon/mailcraft.ico" ] || bad "missing mailcraft.ico"
[ -f "$MC_ROOT/assets/app-icon/mailcraft.icns" ] || bad "missing mailcraft.icns"

if have desktop-file-validate; then
	desktop-file-validate "$desktop" || bad "desktop-file-validate failed"
else
	printf 'check: desktop-file-validate not installed, skipped\n' >&2
fi

if have xmllint; then
	for f in "$p/linux/metainfo.xml.in" "$p/macos/Info.plist.in" "$p/macos/entitlements.plist" "$p/windows/mailcraft.wxs"; do
		xmllint --noout "$f" || bad "$f is not well-formed XML"
	done
fi

if have appstreamcli; then
	scratch="${CARGO_TARGET_DIR:-$MC_ROOT/target}/packaging/check"
	mkdir -p "$scratch"
	sed -e "s|@APP_ID@|$APP_ID|g" -e "s|@SUMMARY@|$APP_SUMMARY|g" -e "s|@LICENSE@|$LICENSE_SPDX|g" \
		-e "s|@HOMEPAGE@|$HOMEPAGE|g" -e "s|@RAW_URL@|$RAW_URL|g" -e "s|@VERSION@|$MC_VERSION|g" \
		-e "s|@DATE@|$MAILCRAFT_BUILD_DATE|g" "$p/linux/metainfo.xml.in" >"$scratch/$APP_ID.metainfo.xml"
	appstreamcli validate --no-net --pedantic "$scratch/$APP_ID.metainfo.xml" ||
		bad "appstreamcli validate failed"
else
	printf 'check: appstreamcli not installed, skipped\n' >&2
fi

# The AppImage library checks must catch what broke the AppImage on Ubuntu 22.04 (GitHub issue #23):
# a fake AppDir whose executable is the system shell bundles nothing, so --check has to fail.
if have readelf; then
	fake="${CARGO_TARGET_DIR:-$MC_ROOT/target}/packaging/check/fake.AppDir"
	rm -rf "$fake"
	mkdir -p "$fake/usr/bin"
	cp "$(readlink -f "$(command -v sh)")" "$fake/usr/bin/mailcraft"
	out="$("$p/linux/appimage-libs.sh" --check "$fake" 2>&1)" && bad "appimage-libs.sh --check passed an AppDir without libraries"
	grep -q 'run-time library libXcursor.so.1 is not bundled' <<<"$out" ||
		bad "appimage-libs.sh --check did not report the missing libXcursor.so.1: $out"
	out="$(MC_APPIMAGE_MAX_GLIBC=2.0 "$p/linux/appimage-libs.sh" --check "$fake" 2>&1)" || true
	grep -q 'needs glibc .*, newer than 2.0' <<<"$out" ||
		bad "appimage-libs.sh --check did not report the glibc limit: $out"
	# Bundled libraries must be found through the executable's RPATH, never through an LD_LIBRARY_PATH that
	# AppRun exports (every program the app opens would inherit it).
	mkdir -p "$fake/usr/lib"
	cp "$fake/usr/bin/mailcraft" "$fake/usr/lib/libXcursor.so.1"
	# shellcheck disable=SC2016 # the AppRun text, not expanded here
	printf '#!/bin/sh\nLD_LIBRARY_PATH="$(dirname "$0")/usr/lib"\nexport LD_LIBRARY_PATH\n' >"$fake/AppRun"
	out="$("$p/linux/appimage-libs.sh" --check "$fake" 2>&1)" || true
	grep -q "usr/bin/mailcraft has RPATH 'none'" <<<"$out" ||
		bad "appimage-libs.sh --check did not report the missing RPATH: $out"
	grep -q 'AppRun sets LD_LIBRARY_PATH' <<<"$out" ||
		bad "appimage-libs.sh --check did not report LD_LIBRARY_PATH in AppRun: $out"
	if have patchelf; then
		patchelf --force-rpath --set-rpath "\$ORIGIN/../lib" "$fake/usr/bin/mailcraft"
		out="$("$p/linux/appimage-libs.sh" --check "$fake" 2>&1)" || true
		if grep -q 'usr/bin/mailcraft has RPATH' <<<"$out"; then
			bad "appimage-libs.sh --check rejected the RPATH \$ORIGIN/../lib: $out"
		fi
	fi
	rm -rf "$fake"
else
	printf 'check: readelf not installed, AppImage library checks skipped\n' >&2
fi

# The AppImage smoke test must notice a bundled library that the running app took from the system (the
# bundle or its RPATH is broken), and one of the libraries it must bundle that was never loaded. It reads
# /proc/PID/maps of the app; here it reads made-up maps of a fake AppImage.
smoke="$p/linux/appimage-smoke.sh"
mapsdir="${CARGO_TARGET_DIR:-$MC_ROOT/target}/packaging/check/maps"
rm -rf "$mapsdir"
bundle="$mapsdir/squashfs-root/usr/lib"
mkdir -p "$bundle"
x11=(libXcursor.so.1 libXi.so.6 libXrender.so.1 libXfixes.so.3 libXext.so.6 libxkbcommon.so.0
	libxkbcommon-x11.so.0 libxcb-xkb.so.1)
for l in "${x11[@]}" libwayland-cursor.so.0; do : >"$bundle/$l"; done
# Writes a maps file $1 that maps each further argument (a path) twice, like a loaded library.
fake_maps() {
	local f="$1" path
	shift
	: >"$f"
	for path in "$@"; do
		printf '7f0000000000-7f0000001000 r--p 00000000 08:01 1234                       %s\n' "$path" >>"$f"
		printf '7f0000001000-7f0000002000 r-xp 00001000 08:01 1234                       %s\n' "$path" >>"$f"
	done
	printf '7ffd00000000-7ffd00021000 rw-p 00000000 00:00 0                          [stack]\n' >>"$f"
}
sys="/usr/lib/x86_64-linux-gnu"
good=()
for l in "${x11[@]}"; do good+=("$bundle/$l"); done
fake_maps "$mapsdir/good" "${good[@]}" "$sys/libX11.so.6.4.0" "$sys/libc.so.6" "$sys/libxcb.so.1.1.0"
"$smoke" --check-maps "$mapsdir/good" "$bundle" >/dev/null 2>&1 ||
	bad "appimage-smoke.sh --check-maps rejected an app that loads its libraries from the AppImage"
# libXext from the system although the AppImage has it; libxkbcommon-x11 from the bundle must not count as
# libxkbcommon.
fake_maps "$mapsdir/bad" "${good[@]/"$bundle/libXext.so.6"/"$sys/libXext.so.6.4.0"}" "$sys/libxkbcommon.so.0.0.0" \
	"$sys/libwayland-cursor.so.0.20.0"
out="$("$smoke" --check-maps "$mapsdir/bad" "$bundle" 2>&1)" &&
	bad "appimage-smoke.sh --check-maps passed an app that loads libXext from the system"
for want in "libXext.so.6 was loaded from $sys/libXext.so.6.4.0" \
	"libxkbcommon.so.0 was loaded from $sys/libxkbcommon.so.0.0.0" \
	"libwayland-cursor.so.0 was loaded from $sys/libwayland-cursor.so.0.20.0"; do
	grep -qF "$want" <<<"$out" || bad "appimage-smoke.sh --check-maps did not report \"$want\": $out"
done
grep -q 'libxkbcommon-x11.so.0 was loaded' <<<"$out" &&
	bad "appimage-smoke.sh --check-maps took the bundled libxkbcommon-x11 for a system copy: $out"
# The bundled libXcursor missing from the map: the app never loaded it (or never opened its window).
fake_maps "$mapsdir/missing" "${good[@]:1}"
out="$("$smoke" --check-maps "$mapsdir/missing" "$bundle" 2>&1)" &&
	bad "appimage-smoke.sh --check-maps passed an app that never loaded libXcursor"
grep -q 'libXcursor.so.1 was never loaded' <<<"$out" ||
	bad "appimage-smoke.sh --check-maps did not report the missing libXcursor.so.1: $out"
rm -rf "$mapsdir"

[ "$fail" = 0 ] || exit 1
printf 'packaging checks passed (%s %s)\n' "$APP_ID" "$MC_VERSION"
