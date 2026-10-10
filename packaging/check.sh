#!/usr/bin/env bash
# Consistency checks for the packaging files (no build needed); run by .github/workflows/packaging-lint.yml.
#
#     packaging/check.sh
#
# - files named after APP_ID exist and carry it (desktop file, Flatpak manifest)
# - the desktop file passes desktop-file-validate and the filled-in AppStream file passes
#   `appstreamcli validate --no-net` (each when the tool is installed)
# - XML templates are well-formed (xmllint, when installed)
# - every icon size the scripts expect is present
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

[ "$fail" = 0 ] || exit 1
printf 'packaging checks passed (%s %s)\n' "$APP_ID" "$MC_VERSION"
