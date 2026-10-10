#!/usr/bin/env bash
# Lays out an installed MailCraft under a prefix folder: the shared step behind the Linux tarball, deb/rpm,
# AppImage and Flatpak, and the FreeBSD tarball.
#
#     packaging/stage-unix.sh <bin-dir> <prefix-dir>
#
#   <bin-dir>     folder with the built `mailcraft` and `mailcraft-cli` (e.g. target/release)
#   <prefix-dir>  folder that plays the role of the prefix (stage/usr, AppDir/usr, /app, …)
#
# Result under <prefix-dir>:
#   bin/mailcraft, bin/mailcraft-cli
#   share/applications/$APP_ID.desktop
#   share/metainfo/$APP_ID.metainfo.xml
#   share/icons/hicolor/<size>/apps/$APP_ID.{png,svg}
#   share/doc/mailcraft/  README, licences, NOTICE, ATTRIBUTION and the bundled fonts' OFL texts
set -euo pipefail
here="$(dirname "${BASH_SOURCE[0]}")"
# shellcheck source=packaging/env.sh
. "$here/env.sh"

[ $# -eq 2 ] || mc_die "usage: $0 <bin-dir> <prefix-dir>"
bin_dir="$1"
prefix="$2"

for b in $APP_BINS; do
	[ -x "$bin_dir/$b" ] || mc_die "missing executable $bin_dir/$b (build it first)"
done

mkdir -p "$prefix/bin" "$prefix/share/applications" "$prefix/share/metainfo" "$prefix/share/doc/mailcraft"
for b in $APP_BINS; do
	install -m 0755 "$bin_dir/$b" "$prefix/bin/$b"
done

desktop="$MC_ROOT/packaging/linux/$APP_ID.desktop"
[ -f "$desktop" ] || mc_die "missing $desktop (the file name must match APP_ID in packaging/env.sh)"
install -m 0644 "$desktop" "$prefix/share/applications/$APP_ID.desktop"

# AppStream: fill the template from env.sh. The release date is the commit date.
sed -e "s|@APP_ID@|$APP_ID|g" \
	-e "s|@SUMMARY@|$APP_SUMMARY|g" \
	-e "s|@LICENSE@|$LICENSE_SPDX|g" \
	-e "s|@HOMEPAGE@|$HOMEPAGE|g" \
	-e "s|@RAW_URL@|$RAW_URL|g" \
	-e "s|@VERSION@|$MC_VERSION|g" \
	-e "s|@DATE@|$MAILCRAFT_BUILD_DATE|g" \
	"$MC_ROOT/packaging/linux/metainfo.xml.in" >"$prefix/share/metainfo/$APP_ID.metainfo.xml"
chmod 0644 "$prefix/share/metainfo/$APP_ID.metainfo.xml"

"$here/icons.sh" "$prefix/share"

for f in README.md LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md; do
	install -m 0644 "$MC_ROOT/$f" "$prefix/share/doc/mailcraft/$f"
done
# The fonts are compiled into the binary (crates/fonts); their licence texts travel with it.
for f in "$MC_ROOT"/assets/fonts/OFL-*.txt; do
	install -m 0644 "$f" "$prefix/share/doc/mailcraft/$(basename "$f")"
done
mc_log "staged MailCraft $MC_VERSION into $prefix"
