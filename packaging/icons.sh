#!/usr/bin/env bash
# Installs the app icon into a freedesktop icon theme folder, renamed to the application id.
#
#     packaging/icons.sh <datadir>        e.g. stage/usr/share, AppDir/usr/share, /app/share
#
# Source: assets/app-icon/hicolor/<size>/apps/mailcraft.{png,svg}, rendered from our own
# assets/app-icon/mailcraft.svg by `cargo xtask ico` (see ATTRIBUTION.md).
# Result: <datadir>/icons/hicolor/<size>/apps/$APP_ID.{png,svg}
set -euo pipefail
# shellcheck source=packaging/env.sh
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

[ $# -eq 1 ] || mc_die "usage: $0 <datadir>"
datadir="$1"
src="$MC_ROOT/assets/app-icon/hicolor"
[ -d "$src" ] || mc_die "missing $src (run cargo xtask ico)"

count=0
for dir in "$src"/*/apps; do
	size="$(basename "$(dirname "$dir")")"
	for icon in "$dir"/mailcraft.png "$dir"/mailcraft.svg; do
		[ -f "$icon" ] || continue
		ext="${icon##*.}"
		dest="$datadir/icons/hicolor/$size/apps"
		mkdir -p "$dest"
		install -m 0644 "$icon" "$dest/$APP_ID.$ext"
		count=$((count + 1))
	done
done
[ "$count" -gt 0 ] || mc_die "no icons found under $src"
mc_log "installed $count icons into $datadir/icons/hicolor"
