#!/usr/bin/env bash
# Starts an AppImage under a fresh Xvfb and checks that it opens its window, keeps running and uses the
# libraries it bundles: the test the AppImage catalog runs, and what a user on a desktop without the app's
# libraries sees.
#
#     packaging/linux/appimage-smoke.sh [--timeout SECONDS] [--settle SECONDS] <MailCraft-ARCH.AppImage>
#     packaging/linux/appimage-smoke.sh --check-maps <MAPS-FILE> <LIBDIR>
#
#   --timeout     how long to wait for the window (default 30 s)
#   --settle      how long the app must keep running once its window is open (default 10 s)
#   --check-maps  only run the library check below on a saved /proc/PID/maps, with LIBDIR as the AppImage's
#                 usr/lib (packaging/check.sh tests the check this way)
#
# Meant for a clean, offline system: packaging/linux/appimage-test.sh runs it inside an Ubuntu 22.04
# container without network. Needs Xvfb and xdotool. The AppImage runs with APPIMAGE_EXTRACT_AND_RUN=1
# (no FUSE needed), a new empty HOME and no arguments, like a first start.
# Exit status 0 when, in this order: a visible MailCraft window appears within the timeout; the app is
# still running when the settle time is over; its environment has no LD_LIBRARY_PATH; and the libraries in
# FROM_BUNDLE below, plus every other library it loaded that the AppImage bundles, were loaded from the
# AppImage's usr/lib. 1 otherwise, after printing the app's output.
set -euo pipefail

# Libraries the app loads on X11 that the test system does not provide, or that the AppImage must bring
# anyway (packaging/linux/appimage-libs.sh bundles them: the dlopen'd libXcursor, libXi, libxkbcommon and
# libxkbcommon-x11, and their dependencies). Each one must show up in the running app's memory map, loaded
# from the AppImage's usr/lib; a copy from the system means the bundle or its RPATH is broken, even when
# the system copy happens to make the app work here.
FROM_BUNDLE=(
	libXcursor.so.1 libXi.so.6 libXrender.so.1 libXfixes.so.3 libXext.so.6
	libxkbcommon.so.0 libxkbcommon-x11.so.0 libxcb-xkb.so.1
)

# True when the file name $1 (a mapped file) is the library with soname $2 or a versioned file of it
# (libXext.so.6.4.0 for libXext.so.6).
is_lib() { [[ $1 == "$2" || $1 == "$2".* ]]; }

# Checks the libraries mapped in the /proc/PID/maps text in file $1 against the AppImage's usr/lib $2:
# every FROM_BUNDLE library and every library the AppImage bundles that is loaded must come from $2, and
# every FROM_BUNDLE library must be loaded. Prints one line per problem; returns 1 when there is one.
check_maps() {
	local maps="$1" libdir="${2%/}" rc=0 path base lib found from
	local -a mapped=() wanted=("${FROM_BUNDLE[@]}")
	while IFS= read -r path; do mapped+=("$path"); done < <(awk '$6 ~ /\.so/ { print $6 }' "$maps" | sort -u)
	for path in "$libdir"/*; do
		[ -f "$path" ] || continue
		base="${path##*/}"
		case " ${wanted[*]} " in
		*" $base "*) ;;
		*) wanted+=("$base") ;;
		esac
	done
	for lib in "${wanted[@]}"; do
		found=0
		for path in "${mapped[@]}"; do
			base="${path##*/}"
			is_lib "$base" "$lib" || continue
			found=1
			from="${path%/*}"
			if [ "$from" != "$libdir" ]; then
				echo "appimage-smoke: $lib was loaded from $path, not from the AppImage ($libdir)" >&2
				rc=1
			fi
		done
		if [ "$found" = 0 ]; then
			case " ${FROM_BUNDLE[*]} " in
			*" $lib "*)
				echo "appimage-smoke: $lib was never loaded (expected from the AppImage's usr/lib)" >&2
				rc=1
				;;
			esac
		fi
	done
	return "$rc"
}

timeout=30
settle=10
image=""
while [ $# -gt 0 ]; do
	case "$1" in
	--timeout) timeout="${2:?--timeout needs a value}"; shift ;;
	--settle) settle="${2:?--settle needs a value}"; shift ;;
	--check-maps)
		[ $# -eq 3 ] || { echo "usage: $0 --check-maps <MAPS-FILE> <LIBDIR>" >&2; exit 2; }
		check_maps "$2" "$3" && echo "appimage-smoke: libraries ok"
		exit $?
		;;
	-h | --help) sed -n '2,20p' "$0"; exit 0 ;;
	-*) echo "appimage-smoke: unknown option $1" >&2; exit 2 ;;
	*) image="$1" ;;
	esac
	shift
done
for n in "$timeout" "$settle"; do
	case "$n" in
	'' | *[!0-9]*) echo "appimage-smoke: $n is not a number of seconds" >&2; exit 2 ;;
	esac
done
[ -n "$image" ] || { echo "usage: $0 [--timeout SECONDS] [--settle SECONDS] <AppImage>" >&2; exit 2; }
[ -f "$image" ] || { echo "appimage-smoke: $image not found" >&2; exit 2; }
for t in Xvfb xdotool; do
	command -v "$t" >/dev/null 2>&1 || { echo "appimage-smoke: $t not installed" >&2; exit 2; }
done

work="$(mktemp -d "${TMPDIR:-/tmp}/appimage-smoke.XXXXXX")"
xvfb_pid=""
app_pid=""
# Called through the EXIT trap.
# shellcheck disable=SC2317
cleanup() {
	if [ -n "$app_pid" ]; then kill "$app_pid" 2>/dev/null || true; fi
	if [ -n "$xvfb_pid" ]; then kill "$xvfb_pid" 2>/dev/null || true; fi
	rm -rf "$work"
}
trap cleanup EXIT
# A copy, so a read-only or non-executable file (a downloaded CI artifact) runs too.
cp "$image" "$work/app.AppImage"
chmod 0755 "$work/app.AppImage"
image="$work/app.AppImage"

display=":${SMOKE_DISPLAY:-97}"
Xvfb "$display" -screen 0 1280x1024x24 -nolisten tcp >"$work/xvfb.log" 2>&1 &
xvfb_pid=$!
for _ in $(seq 1 50); do
	DISPLAY="$display" xdotool getmouselocation >/dev/null 2>&1 && break
	sleep 0.1
done

mkdir -p "$work/home" "$work/run"
chmod 0700 "$work/run"
(
	cd "$work"
	exec env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$work/home" XDG_RUNTIME_DIR="$work/run" \
		DISPLAY="$display" APPIMAGE_EXTRACT_AND_RUN=1 TMPDIR="$work" LANG=C.UTF-8 "$image"
) >"$work/app.log" 2>&1 &
app_pid=$!

found=""
for _ in $(seq 1 "$((timeout * 2))"); do
	if ! kill -0 "$app_pid" 2>/dev/null; then
		break
	fi
	found="$(DISPLAY="$display" xdotool search --onlyvisible --name 'MailCraft' 2>/dev/null | head -n 1 || true)"
	[ -n "$found" ] && break
	sleep 0.5
done

# Process id of the running app (the process whose executable is .../usr/bin/mailcraft), or nothing.
app_proc() {
	local p exe
	for p in /proc/[0-9]*; do
		exe="$(readlink "$p/exe" 2>/dev/null || true)"
		case "$exe" in
		*/usr/bin/mailcraft) echo "${p#/proc/}"; return 0 ;;
		esac
	done
}

status=0
fail() {
	echo "appimage-smoke: $*" >&2
	status=1
}
# Sets code to the exit status of the app (once it has exited) and forgets its process id.
code=0
reap() {
	code=0
	wait "$app_pid" || code=$?
	app_pid=""
}
if [ -z "$found" ]; then
	if kill -0 "$app_pid" 2>/dev/null; then
		fail "no MailCraft window after $timeout s"
	else
		reap
		fail "the app exited (status $code) before showing a window"
	fi
else
	echo "appimage-smoke: window \"$(DISPLAY="$display" xdotool getwindowname "$found" 2>/dev/null || true)\" is open: ok"
	# A crash right after the first frame (a background thread on first start, the first-run setup in
	# the empty HOME, the second frame) must fail the test too.
	for _ in $(seq 1 "$((settle * 2))"); do
		kill -0 "$app_pid" 2>/dev/null || break
		sleep 0.5
	done
	pid="$(app_proc)"
	if ! kill -0 "$app_pid" 2>/dev/null; then
		reap
		fail "the app exited (status $code) within $settle s after its window appeared"
	elif [ -z "$pid" ]; then
		fail "the app's usr/bin/mailcraft process is gone $settle s after its window appeared"
	else
		echo "appimage-smoke: still running $settle s later: ok"
		# The app is started with an empty environment, so an LD_LIBRARY_PATH here came from the AppImage
		# and would reach every program the app opens (browser, attachment viewers).
		ld_path="$(tr '\0' '\n' <"/proc/$pid/environ" 2>/dev/null | sed -n 's/^LD_LIBRARY_PATH=//p' || true)"
		if [ -n "$ld_path" ]; then
			fail "the app runs with LD_LIBRARY_PATH=$ld_path, which programs it opens would inherit"
		fi
		exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
		cat "/proc/$pid/maps" >"$work/maps" 2>/dev/null || true
		if check_maps "$work/maps" "${exe%/bin/mailcraft}/lib"; then
			echo "appimage-smoke: bundled libraries loaded from the AppImage: ok"
		else
			status=1
		fi
	fi
fi
if [ "$status" != 0 ]; then
	echo "--- app output:" >&2
	head -n 80 "$work/app.log" >&2 || true
fi
exit "$status"
