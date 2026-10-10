#!/usr/bin/env bash
# macOS: universal (Apple silicon + Intel) MailCraft.app in a DMG. Runs on a Mac (self-hosted `macos`
# runner) with Xcode command-line tools and rustup.
#
#     packaging/macos/package.sh [--out DIR] [--no-build]
#
# Steps: cargo build --release for aarch64-apple-darwin and x86_64-apple-darwin → lipo → MailCraft.app
# (Contents/MacOS/mailcraft and mailcraft-cli, Resources/mailcraft.icns, Info.plist from Info.plist.in) →
# codesign → DMG (hdiutil) → notarise and staple.
#
# Signing and notarisation happen only when the secrets are present; otherwise the app is ad-hoc signed
# (runs locally; Gatekeeper asks on other Macs):
#   APPLE_CERT_P12           base64 of a "Developer ID Application" certificate + key (.p12)
#   APPLE_CERT_PASSWORD      its password
#   APPLE_SIGNING_IDENTITY   e.g. "Developer ID Application: Name (TEAMID)"
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD   (app-specific password) for `xcrun notarytool`
#
# Output: MailCraft-VERSION-macos-universal.dmg
set -euo pipefail
here="$(dirname "${BASH_SOURCE[0]}")"
# shellcheck source=packaging/env.sh
. "$here/../env.sh"

out="$MC_DIST"
build=1
while [ $# -gt 0 ]; do
	case "$1" in
	--out) out="${2:?--out needs a value}"; shift ;;
	--no-build) build=0 ;;
	-h | --help) sed -n '2,21p' "$0"; exit 0 ;;
	*) mc_die "unknown option $1" ;;
	esac
	shift
done

targets="aarch64-apple-darwin x86_64-apple-darwin"
if [ "$build" = 1 ]; then
	if command -v rustup >/dev/null 2>&1; then
		# shellcheck disable=SC2086 # two words on purpose
		rustup target add $targets
	fi
	for t in $targets; do
		mc_log "cargo build --release --target $t (git $MAILCRAFT_GIT_HASH, $MAILCRAFT_BUILD_DATE)"
		(cd "$MC_ROOT" && MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --locked --release --target "$t" \
			-p mailcraft -p mailcraft-cli)
	done
fi

work="${CARGO_TARGET_DIR:-$MC_ROOT/target}/packaging/macos"
app="$work/MailCraft.app"
rm -rf "$work"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/licences" "$out"
out="$(cd "$out" && pwd)"

for b in $APP_BINS; do
	slices=""
	for t in $targets; do
		f="$(mc_bin_dir "$t" release)/$b"
		[ -x "$f" ] || mc_die "missing $f"
		slices="$slices $f"
	done
	# shellcheck disable=SC2086 # one path per slice
	lipo -create -output "$app/Contents/MacOS/$b" $slices
	lipo -info "$app/Contents/MacOS/$b" >&2
done

build_number="$(git -C "$MC_ROOT" rev-list --count HEAD 2>/dev/null || echo 1)"
sed -e "s|@APP_ID@|$APP_ID|g" -e "s|@VERSION@|${MC_VERSION%%[-+]*}|g" -e "s|@BUILD@|$build_number|g" \
	"$here/Info.plist.in" >"$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >&2
cp "$MC_ROOT/assets/app-icon/mailcraft.icns" "$app/Contents/Resources/mailcraft.icns"
for f in README.md LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md "$MC_ROOT"/assets/fonts/OFL-*.txt; do
	case "$f" in /*) cp "$f" "$app/Contents/Resources/licences/" ;; *) cp "$MC_ROOT/$f" "$app/Contents/Resources/licences/" ;; esac
done

identity="${APPLE_SIGNING_IDENTITY:-}"
keychain=""
cleanup() {
	if [ -n "$keychain" ]; then
		security delete-keychain "$keychain" >/dev/null 2>&1 || true
	fi
}
trap cleanup EXIT

if [ -n "${APPLE_CERT_P12:-}" ] && [ -n "${APPLE_CERT_PASSWORD:-}" ] && [ -n "$identity" ]; then
	mc_log "importing the signing certificate into a temporary keychain"
	keychain="$work/signing.keychain-db"
	kc_pass="$(openssl rand -hex 16)"
	printf '%s' "$APPLE_CERT_P12" | base64 --decode >"$work/cert.p12"
	security create-keychain -p "$kc_pass" "$keychain"
	security set-keychain-settings -lut 3600 "$keychain"
	security unlock-keychain -p "$kc_pass" "$keychain"
	security import "$work/cert.p12" -k "$keychain" -P "$APPLE_CERT_PASSWORD" -T /usr/bin/codesign
	security set-key-partition-list -S apple-tool:,apple: -s -k "$kc_pass" "$keychain" >/dev/null
	# shellcheck disable=SC2046 # the existing list, one keychain per word
	security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
	rm -f "$work/cert.p12"
	sign=(codesign --force --timestamp --options runtime --entitlements "$here/entitlements.plist"
		--keychain "$keychain" --sign "$identity")
	"${sign[@]}" "$app/Contents/MacOS/mailcraft-cli"
	"${sign[@]}" "$app"
	codesign --verify --strict --verbose=2 "$app"
else
	mc_log "no signing secrets: ad-hoc signature"
	codesign --force --deep --sign - "$app"
	identity=""
fi

dmg_root="$work/dmg"
mkdir -p "$dmg_root"
cp -R "$app" "$dmg_root/"
ln -s /Applications "$dmg_root/Applications"
dmg="$out/MailCraft-$MC_VERSION-macos-universal.dmg"
rm -f "$dmg"
hdiutil create -volname "MailCraft $MC_VERSION" -srcfolder "$dmg_root" -ov -format UDZO "$dmg"

if [ -n "$identity" ]; then
	codesign --force --timestamp --keychain "$keychain" --sign "$identity" "$dmg"
	if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
		mc_log "notarising (this waits for Apple)"
		xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
			--password "$APPLE_APP_PASSWORD" --wait
		xcrun stapler staple "$dmg"
	else
		mc_log "no APPLE_ID/APPLE_TEAM_ID/APPLE_APP_PASSWORD: signed but not notarised"
	fi
fi
mc_log "wrote $dmg"
