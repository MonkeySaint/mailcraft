#!/usr/bin/env bash
# Web build of MailCraft (apps/mailcraft-web) as a zip of static files.
#
#     packaging/web/package.sh [--out DIR]
#
# With Trunk on PATH: `trunk build --release` (the documented way, apps/mailcraft-web/README.md).
# Without it: cargo build --release --target wasm32-unknown-unknown, then wasm-bindgen --target web (the
# wasm-bindgen-cli version must equal the wasm-bindgen crate in Cargo.lock; it is installed with
# `cargo install` when missing), and an index.html made from the Trunk one with the Trunk links replaced.
# packaging/web/_headers is added in both cases.
#
# Output: mailcraft-VERSION-web.zip (one folder, mailcraft-VERSION-web/, to serve as the site root).
set -euo pipefail
here="$(dirname "${BASH_SOURCE[0]}")"
# shellcheck source=packaging/env.sh
. "$here/../env.sh"

out="$MC_DIST"
while [ $# -gt 0 ]; do
	case "$1" in
	--out) out="${2:?--out needs a value}"; shift ;;
	-h | --help) sed -n '2,13p' "$0"; exit 0 ;;
	*) mc_die "unknown option $1" ;;
	esac
	shift
done

web="$MC_ROOT/apps/mailcraft-web"
target_dir="${CARGO_TARGET_DIR:-$MC_ROOT/target}"
work="$target_dir/packaging/web"
name="mailcraft-$MC_VERSION-web"
site="$work/$name"
rm -rf "$work"
mkdir -p "$site" "$out"
out="$(cd "$out" && pwd)"

if command -v rustup >/dev/null 2>&1; then
	rustup target add wasm32-unknown-unknown
fi

if command -v trunk >/dev/null 2>&1; then
	mc_log "trunk build --release (git $MAILCRAFT_GIT_HASH, $MAILCRAFT_BUILD_DATE)"
	(cd "$web" && trunk build --release --dist "$site")
else
	mc_log "trunk not found: cargo + wasm-bindgen"
	(cd "$MC_ROOT" && cargo build --locked --release --target wasm32-unknown-unknown -p mailcraft-web)
	want="$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' "$MC_ROOT/Cargo.lock")"
	[ -n "$want" ] || mc_die "wasm-bindgen not found in Cargo.lock"
	if ! wasm-bindgen --version 2>/dev/null | grep -q " $want\$"; then
		mc_log "installing wasm-bindgen-cli $want"
		cargo install --locked wasm-bindgen-cli --version "$want"
	fi
	wasm-bindgen --target web --no-typescript --out-dir "$site" \
		"$target_dir/wasm32-unknown-unknown/release/mailcraft-web.wasm"
	cp "$MC_ROOT/assets/app-icon/hicolor/128x128/apps/mailcraft.png" "$site/mailcraft.png"
	# Replace the two Trunk directives: the Rust link becomes the wasm-bindgen loader, the copy-file link
	# is done above.
	awk '
		/<link data-trunk rel="rust"/ {
			print "  <script type=\"module\">import init from \"./mailcraft-web.js\"; init();</script>"
			next
		}
		/<link data-trunk rel="copy-file"/ { next }
		{ print }
	' "$web/index.html" >"$site/index.html"
	grep -q 'data-trunk' "$site/index.html" && mc_die "index.html still has Trunk directives"
fi

[ -f "$site/index.html" ] || mc_die "no index.html in $site"
ls "$site"/*.wasm >/dev/null 2>&1 || mc_die "no .wasm in $site"
cp "$here/_headers" "$site/_headers"
cp "$MC_ROOT/LICENSE-MIT" "$MC_ROOT/LICENSE-APACHE" "$MC_ROOT/NOTICE" "$site/"

zip_file="$out/$name.zip"
rm -f "$zip_file"
(cd "$work" && { find "$name" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} + 2>/dev/null || true; } && zip -qrX "$zip_file" "$name")
mc_log "wrote $zip_file"
