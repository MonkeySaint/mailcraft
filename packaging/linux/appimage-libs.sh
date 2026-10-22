#!/usr/bin/env bash
# Copies the shared libraries the app needs at run time into an AppDir (usr/lib), then checks the AppDir.
#
#     packaging/linux/appimage-libs.sh [--arch x86_64|aarch64] [--check] <AppDir>
#
#   --arch    architecture of the AppDir (default: the host's)
#   --check   copy nothing; only run the checks below on an AppDir that is already filled
#
# Why: the app links almost nothing; winit, xkbcommon-dl, wayland-sys and glutin open the windowing
# libraries with dlopen when the window is created. A desktop that lacks one of them (base Ubuntu 22.04
# has no libXcursor.so.1 or libxkbcommon-x11.so.0) used to make the AppImage quit with "Failed to load one
# of xlib's shared libraries" (GitHub issue #23).
#
# What is bundled: every library in RUNTIME_LIBS below and every library that the executables or a
# bundled library list as NEEDED, followed recursively, except the ones on the AppImage excludelist
# (https://github.com/AppImage/pkg2appimage/blob/master/excludelist): glibc, libGL/libEGL/libGLX, libX11,
# libX11-xcb, libxcb, libwayland-client, libz and the rest are always the system's, because they must
# match its drivers and display server. An excluded library's own dependencies are not followed.
#
# Libraries are taken from the build system: /usr/lib/<arch>-linux-gnu (Debian and Ubuntu, also for a
# cross build with the foreign-architecture packages installed), then /usr/lib64 and /usr/lib. The release
# job installs them with packaging/linux/ci-deps.sh. On Debian-based systems the copyright file of each
# library's package goes to usr/share/doc/mailcraft/third-party/.
#
# How the app finds them: patchelf gives every executable in usr/bin the DT_RPATH $ORIGIN/../lib. The
# executable's DT_RPATH applies to every library loaded into the process that has no DT_RUNPATH of its own,
# dlopen included, so the bundled libraries and their dependencies resolve from usr/lib first. Unlike
# LD_LIBRARY_PATH it is not inherited by the programs the app starts (the browser for a link, the viewer
# for an attachment), which must keep using the system's own, possibly newer, libraries.
#
# Checks (both modes; any failure is an error):
#   - every RUNTIME_LIBS entry is excluded or present in usr/lib
#   - every NEEDED entry of every ELF file under usr/ is excluded or present in usr/lib
#   - every bundled library is built for --arch and has no DT_RUNPATH (which would hide the RPATH)
#   - every executable in usr/bin has the DT_RPATH $ORIGIN/../lib when usr/lib holds libraries
#   - AppRun does not set LD_LIBRARY_PATH
#   - the newest glibc symbol version referenced is at most MC_APPIMAGE_MAX_GLIBC (default 2.35, the
#     glibc of Ubuntu 22.04, the oldest system the AppImage supports)
set -euo pipefail
# shellcheck source=packaging/env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

# Opened with dlopen at run time (crate → sonames):
#   winit X11 (x11-dl)        libX11.so.6 libX11-xcb.so.1 libXcursor.so.1 libXi.so.6
#   xkbcommon-dl              libxkbcommon.so.0 libxkbcommon-x11.so.0
#   wayland-sys               libwayland-client.so.0 libwayland-cursor.so.0 libwayland-egl.so.1
#   glutin                    libEGL.so.1 libGL.so.1
RUNTIME_LIBS=(
	libX11.so.6 libX11-xcb.so.1 libXcursor.so.1 libXi.so.6
	libxkbcommon.so.0 libxkbcommon-x11.so.0
	libwayland-client.so.0 libwayland-cursor.so.0 libwayland-egl.so.1
	libEGL.so.1 libGL.so.1
)

# The sonames of the AppImage excludelist (see the link above): never bundled, always the system's.
EXCLUDED=(
	ld-linux.so.2 ld-linux-x86-64.so.2 ld-linux-aarch64.so.1 libanl.so.1 libBrokenLocale.so.1 libcidn.so.1
	libc.so.6 libdl.so.2 libm.so.6 libmvec.so.1 libnss_compat.so.2 libnss_dns.so.2 libnss_files.so.2
	libnss_hesiod.so.2 libnss_nisplus.so.2 libnss_nis.so.2 libpthread.so.0 libresolv.so.2 librt.so.1
	libthread_db.so.1 libutil.so.1 libstdc++.so.6 libGL.so.1 libEGL.so.1 libGLdispatch.so.0 libGLX.so.0
	libOpenGL.so.0 libdrm.so.2 libglapi.so.0 libgbm.so.1 libxcb.so.1 libX11.so.6 libX11-xcb.so.1
	libwayland-client.so.0 libasound.so.2 libfontconfig.so.1 libfreetype.so.6 libharfbuzz.so.0
	libcom_err.so.2 libexpat.so.1 libgcc_s.so.1 libgpg-error.so.0 libICE.so.6 libSM.so.6
	libusb-1.0.so.0 libuuid.so.1 libz.so.1 libjack.so.0 libpipewire-0.3.so.0 libxcb-dri3.so.0
	libxcb-dri2.so.0 libfribidi.so.0 libgmp.so.10
)

arch="$(uname -m)"
check_only=0
appdir=""
while [ $# -gt 0 ]; do
	case "$1" in
	--arch) arch="${2:?--arch needs a value}"; shift ;;
	--check) check_only=1 ;;
	-h | --help) sed -n '2,38p' "$0"; exit 0 ;;
	-*) mc_die "unknown option $1" ;;
	*) [ -z "$appdir" ] || mc_die "only one AppDir"; appdir="$1" ;;
	esac
	shift
done
[ -n "$appdir" ] || mc_die "usage: $0 [--arch ARCH] [--check] <AppDir>"
[ -d "$appdir/usr/bin" ] || mc_die "$appdir/usr/bin is missing (stage the AppDir first)"
case "$arch" in
x86_64 | amd64) arch="x86_64"; machine="X86-64" ;;
aarch64 | arm64) arch="aarch64"; machine="AArch64" ;;
*) mc_die "unsupported architecture $arch" ;;
esac
command -v readelf >/dev/null 2>&1 || mc_die "readelf not found (install binutils)"
max_glibc="${MC_APPIMAGE_MAX_GLIBC:-2.35}"
libdir="$appdir/usr/lib"
# The literal string the dynamic loader expands; it must not be expanded here.
# shellcheck disable=SC2016
want_rpath='$ORIGIN/../lib'

is_excluded() {
	local x
	for x in "${EXCLUDED[@]}"; do
		[ "$x" = "$1" ] && return 0
	done
	return 1
}

is_elf() { [ -f "$1" ] && [ "$(head -c 4 "$1" | od -An -c | tr -d ' ')" = '177ELF' ]; }

# NEEDED entries of an ELF file, one per line.
needed() { readelf -d -W "$1" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'; }
# The DT_RPATH or DT_RUNPATH ($2 = RPATH or RUNPATH) of an ELF file, empty when it has none.
dyn_path() { readelf -d -W "$1" 2>/dev/null | sed -n "s/.*($2).*\\[\\(.*\\)\\]\$/\\1/p"; }

# True when the ELF file is a 64-bit object for $arch.
right_arch() {
	local h
	h="$(readelf -h -W "$1" 2>/dev/null)" || return 1
	grep -q 'Class:[[:space:]]*ELF64' <<<"$h" && grep -q "Machine:.*$machine" <<<"$h"
}

# Path of a library on the build system, for $arch.
find_lib() {
	local d
	for d in "/usr/lib/$arch-linux-gnu" "/lib/$arch-linux-gnu" /usr/lib64 /lib64 /usr/lib /lib; do
		if [ -e "$d/$1" ] && right_arch "$d/$1"; then
			echo "$d/$1"
			return 0
		fi
	done
	return 1
}

# Copies the copyright file of the Debian package that owns a library (no-op elsewhere).
copy_copyright() {
	command -v dpkg-query >/dev/null 2>&1 || return 0
	local path="$1" pkg real doc
	real="$(readlink -f "$path")"
	pkg="$(dpkg-query -S "$real" 2>/dev/null || dpkg-query -S "$path" 2>/dev/null ||
		dpkg-query -S "${real#/usr}" 2>/dev/null || true)"
	pkg="${pkg%%:*}"
	[ -n "$pkg" ] || mc_die "no Debian package owns $path (needed for its copyright file)"
	doc="/usr/share/doc/$pkg/copyright"
	[ -f "$doc" ] || mc_die "missing $doc (the licence of bundled $(basename "$path"))"
	mkdir -p "$appdir/usr/share/doc/mailcraft/third-party"
	install -m 0644 "$doc" "$appdir/usr/share/doc/mailcraft/third-party/$pkg.copyright"
}

if [ "$check_only" = 0 ]; then
	mkdir -p "$libdir"
	queue=("${RUNTIME_LIBS[@]}")
	for f in "$appdir"/usr/bin/*; do
		is_elf "$f" || continue
		while IFS= read -r n; do queue+=("$n"); done < <(needed "$f")
	done
	missing=()
	i=0
	while [ "$i" -lt "${#queue[@]}" ]; do
		name="${queue[$i]}"
		i=$((i + 1))
		is_excluded "$name" && continue
		[ -e "$libdir/$name" ] && continue
		if ! src="$(find_lib "$name")"; then
			missing+=("$name")
			continue
		fi
		install -m 0644 "$(readlink -f "$src")" "$libdir/$name"
		copy_copyright "$src"
		mc_log "bundled $name (from $src)"
		while IFS= read -r n; do queue+=("$n"); done < <(needed "$src")
	done
	[ "${#missing[@]}" = 0 ] ||
		mc_die "libraries for $arch not found on this system: ${missing[*]} (packaging/linux/ci-deps.sh installs them)"
	if [ -n "$(find "$libdir" -maxdepth 1 -type f -print -quit)" ]; then
		command -v patchelf >/dev/null 2>&1 || mc_die "patchelf not found (install the patchelf package)"
		for f in "$appdir"/usr/bin/*; do
			is_elf "$f" || continue
			# --force-rpath writes DT_RPATH, which (unlike DT_RUNPATH) also covers the libraries that the
			# bundled ones load, and dlopen from any of them.
			patchelf --force-rpath --set-rpath "$want_rpath" "$f"
			mc_log "set RPATH $want_rpath on usr/bin/$(basename "$f")"
		done
	fi
fi

# Checks.
fail=0
bad() {
	printf 'appimage-libs: %s\n' "$*" >&2
	fail=1
}
for name in "${RUNTIME_LIBS[@]}"; do
	is_excluded "$name" || [ -f "$libdir/$name" ] || bad "run-time library $name is not bundled"
done
have_libs=0
[ -d "$libdir" ] && [ -n "$(find "$libdir" -maxdepth 1 -type f -print -quit)" ] && have_libs=1
if [ "$have_libs" = 1 ]; then
	for f in "$appdir"/usr/bin/*; do
		is_elf "$f" || continue
		r="$(dyn_path "$f" RPATH)"
		[ "$r" = "$want_rpath" ] ||
			bad "usr/bin/$(basename "$f") has RPATH '${r:-none}', not '$want_rpath' (the bundled libraries would not be found)"
		[ -z "$(dyn_path "$f" RUNPATH)" ] || bad "usr/bin/$(basename "$f") has a RUNPATH, which hides its RPATH"
	done
fi
# LD_LIBRARY_PATH would reach every program the app starts and load the bundled libraries into them.
if [ -f "$appdir/AppRun" ] && grep -q 'LD_LIBRARY_PATH' "$appdir/AppRun"; then
	bad "AppRun sets LD_LIBRARY_PATH (programs the app opens would inherit it; the RPATH finds the libraries)"
fi
newest=""
while IFS= read -r -d '' f; do
	is_elf "$f" || continue
	case "$f" in
	"$libdir"/*)
		right_arch "$f" || bad "$(basename "$f") is not a 64-bit $arch library"
		[ -z "$(dyn_path "$f" RUNPATH)" ] || bad "$(basename "$f") has a RUNPATH, which hides the executable's RPATH"
		;;
	esac
	while IFS= read -r n; do
		is_excluded "$n" || [ -f "$libdir/$n" ] || bad "${f#"$appdir"/} needs $n, which is neither bundled nor excluded"
	done < <(needed "$f")
	v="$(readelf -V -W "$f" 2>/dev/null | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -Vu | tail -n 1 || true)"
	if [ -n "$v" ]; then
		newest="$(printf '%s\n%s\n' "$newest" "$v" | sed '/^$/d' | sort -V | tail -n 1)"
		if [ "$(printf '%s\n%s\n' "$v" "$max_glibc" | sort -V | tail -n 1)" != "$max_glibc" ]; then
			bad "${f#"$appdir"/} needs glibc $v, newer than $max_glibc (MC_APPIMAGE_MAX_GLIBC)"
		fi
	fi
done < <(find "$appdir/usr" -type f -print0)
[ "$fail" = 0 ] || mc_die "the AppDir $appdir fails the library checks"
bundled="$(find "$libdir" -maxdepth 1 -type f -printf '%f\n' 2>/dev/null | sort | paste -sd ' ' -)"
mc_log "AppDir libraries ok: bundled ${bundled:-nothing}; newest glibc symbol ${newest:-none} (limit $max_glibc)"
