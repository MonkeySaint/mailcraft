#!/usr/bin/env bash
# Runs packaging/linux/appimage-smoke.sh on an AppImage inside a plain Ubuntu 22.04 container without
# network (packaging/linux/appimage-test/Containerfile), with podman or docker.
#
#     packaging/linux/appimage-test.sh [--timeout SECONDS] [--settle SECONDS] [--self-test] <MailCraft-ARCH.AppImage>
#
#   --self-test  also prove that the smoke test catches what it is meant to catch: it must fail when the
#                app exits a moment after its window appears, and when a bundled library is taken from the
#                system instead of the AppImage (libXext.so.6, which the test image has).
#
# The image (mailcraft-appimage-test) is built on first use, which needs network; the test itself runs with
# --network none. The container's architecture is the host's, so test the AppImage of the host
# architecture. Set CONTAINER=docker or CONTAINER=podman to pick the engine (default: podman if installed).
set -euo pipefail
# shellcheck source=packaging/env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

args=()
image=""
self_test=0
while [ $# -gt 0 ]; do
	case "$1" in
	--timeout) args+=(--timeout "${2:?--timeout needs a value}"); shift ;;
	--settle) args+=(--settle "${2:?--settle needs a value}"); shift ;;
	--self-test) self_test=1 ;;
	-h | --help) sed -n '2,13p' "$0"; exit 0 ;;
	-*) mc_die "unknown option $1" ;;
	*) image="$1" ;;
	esac
	shift
done
[ -n "$image" ] || mc_die "usage: $0 [--timeout SECONDS] [--settle SECONDS] [--self-test] <AppImage>"
[ -f "$image" ] || mc_die "$image not found"
image="$(readlink -f "$image")"

engine="${CONTAINER:-}"
if [ -z "$engine" ]; then
	if command -v podman >/dev/null 2>&1; then engine=podman; else engine=docker; fi
fi
command -v "$engine" >/dev/null 2>&1 || mc_die "$engine not found (install podman or docker)"

tag="mailcraft-appimage-test"
kit="$MC_ROOT/packaging/linux/appimage-test"
mc_log "$engine build $tag"
if ! build_log="$("$engine" build -q -t "$tag" -f "$kit/Containerfile" "$kit" 2>&1)"; then
	printf '%s\n' "$build_log" >&2
	mc_die "could not build the $tag image"
fi

# Runs the smoke test in the container on /test/$1: the AppImage itself, or a case script from $cases.
smoke() {
	local mounts=(-v "$image:/test/app.AppImage:ro" -v "$MC_ROOT/packaging/linux/appimage-smoke.sh:/test/smoke.sh:ro")
	[ -n "${cases:-}" ] && mounts+=(-v "$cases:/test/cases:ro")
	"$engine" run --rm --network none "${mounts[@]}" "$tag" bash /test/smoke.sh "${args[@]}" "/test/$1"
}

mc_log "$(basename "$image") under Xvfb in Ubuntu 22.04, offline"
smoke app.AppImage
[ "$self_test" = 1 ] || exit 0

# Broken variants of the same AppImage; the smoke test passes each one as the AppImage to start. They run
# with the smoke test's environment (TMPDIR is its private work directory, DISPLAY its Xvfb).
cases="${CARGO_TARGET_DIR:-$MC_ROOT/target}/packaging/appimage-self-test"
rm -rf "$cases"
mkdir -p "$cases"
# The app opens its window, then dies a second later.
cat >"$cases/exits-after-window.sh" <<'EOF'
#!/bin/bash
cp /test/app.AppImage "$TMPDIR/real.AppImage"
chmod 0755 "$TMPDIR/real.AppImage"
"$TMPDIR/real.AppImage" &
for _ in $(seq 1 120); do
	[ -n "$(xdotool search --onlyvisible --name MailCraft 2>/dev/null)" ] && break
	sleep 0.5
done
sleep 1
for p in /proc/[0-9]*; do
	case "$(readlink "$p/exe" 2>/dev/null)" in
	*/usr/bin/mailcraft) kill -KILL "${p#/proc/}" ;;
	esac
done
wait
exit 1
EOF
# The AppImage without its libXext.so.6: the app still starts (the test image has libXext from Mesa and
# xdotool), but with the system's copy.
cat >"$cases/system-libxext.sh" <<'EOF'
#!/bin/bash
set -e
cd "$TMPDIR"
cp /test/app.AppImage real.AppImage
chmod 0755 real.AppImage
./real.AppImage --appimage-extract >/dev/null
rm squashfs-root/usr/lib/libXext.so.6
exec squashfs-root/AppRun
EOF
chmod 0755 "$cases"/*.sh

failed=0
# Runs a broken case; it must fail with a message matching $2.
expect_failure() {
	local out
	mc_log "self-test: $1 must fail"
	if out="$(smoke "cases/$1" 2>&1)"; then
		printf '%s\n' "$out" >&2
		printf 'appimage-test: the smoke test passed %s\n' "$1" >&2
		failed=1
	elif ! grep -q "$2" <<<"$out"; then
		printf '%s\n' "$out" >&2
		printf 'appimage-test: the smoke test failed %s without saying "%s"\n' "$1" "$2" >&2
		failed=1
	else
		mc_log "self-test: $1 failed as it should: $(grep -m 1 "$2" <<<"$out")"
	fi
}
expect_failure exits-after-window.sh 'after its window appeared'
expect_failure system-libxext.sh 'libXext.so.6 was loaded from /usr/lib/'
rm -rf "$cases"
[ "$failed" = 0 ] || mc_die "the smoke test misses a broken AppImage"
mc_log "self-test passed"
