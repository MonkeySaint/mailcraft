#!/usr/bin/env bash
# Installs what the Linux release job needs inside the Debian (rust:*-bookworm) container (as root).
#
#     packaging/linux/ci-deps.sh [TARGET-TRIPLE]
#
# - apt: packaging tools (zsync, desktop-file-utils, appstream, file, zip, jq, binutils, patchelf), the
#   windowing libraries that packaging/linux/appimage-libs.sh copies into the AppImage (for TRIPLE's arch)
#   and, when TRIPLE is for a different architecture than the container, the GNU cross compiler for it.
# - nfpm NFPM_VERSION (MIT) from its GitHub release, checked against the release's checksums file and
#   cached in $MC_TOOL_CACHE.
# - With a CI env file ($GITHUB_ENV), the cross linker settings for cargo and the cc crate, and the cache
#   folder on PATH ($GITHUB_PATH).
set -euo pipefail
# shellcheck source=packaging/env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

triple="${1:-}"
nfpm_version="${NFPM_VERSION:-2.43.0}"
cache="${MC_TOOL_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/mailcraft-packaging}"
env_file="${GITHUB_ENV:-/dev/null}"
path_file="${GITHUB_PATH:-/dev/null}"

host="$(uname -m)"
want="${triple%%-*}"
[ -n "$want" ] || want="$host"

pkgs=(zsync desktop-file-utils appstream file zip jq curl ca-certificates binutils patchelf
	libxkbcommon-dev libwayland-dev libgl1-mesa-dev)
# Run-time libraries bundled into the AppImage (see RUNTIME_LIBS in appimage-libs.sh).
runtime_pkgs=(libx11-6 libx11-xcb1 libxcursor1 libxi6 libxkbcommon0 libxkbcommon-x11-0
	libwayland-client0 libwayland-cursor0 libwayland-egl1)
cross=""
deb_arch=""
if [ "$want" != "$host" ]; then
	case "$want" in
	aarch64) cross="aarch64-linux-gnu"; deb_arch="arm64"; pkgs+=(gcc-aarch64-linux-gnu libc6-dev-arm64-cross) ;;
	x86_64) cross="x86_64-linux-gnu"; deb_arch="amd64"; pkgs+=(gcc-x86-64-linux-gnu libc6-dev-amd64-cross) ;;
	*) mc_die "no cross toolchain known for $want" ;;
	esac
fi
for p in "${runtime_pkgs[@]}"; do
	pkgs+=("$p${deb_arch:+:$deb_arch}")
done

export DEBIAN_FRONTEND=noninteractive
if [ -n "$deb_arch" ]; then dpkg --add-architecture "$deb_arch"; fi
apt-get update -q
apt-get install -y -q --no-install-recommends "${pkgs[@]}"

if [ -n "$cross" ]; then
	triple_env="$(echo "$triple" | tr '[:lower:]-' '[:upper:]_')"
	triple_cc="$(echo "$triple" | tr '-' '_')"
	{
		echo "CARGO_TARGET_${triple_env}_LINKER=$cross-gcc"
		echo "CC_${triple_cc}=$cross-gcc"
		echo "AR_${triple_cc}=$cross-ar"
	} >>"$env_file"
	mc_log "cross-compiling for $triple with $cross-gcc"
fi

case "$host" in
x86_64) nfpm_arch="x86_64" ;;
aarch64) nfpm_arch="arm64" ;;
*) mc_die "no nfpm build for $host" ;;
esac
mkdir -p "$cache/bin"
if [ ! -x "$cache/bin/nfpm" ] || ! "$cache/bin/nfpm" --version 2>/dev/null | grep -q "$nfpm_version"; then
	base="https://github.com/goreleaser/nfpm/releases/download/v$nfpm_version"
	file="nfpm_${nfpm_version}_Linux_$nfpm_arch.tar.gz"
	tmp="$cache/nfpm-download"
	rm -rf "$tmp"
	mkdir -p "$tmp"
	curl -fsSL --retry 3 -o "$tmp/$file" "$base/$file"
	curl -fsSL --retry 3 -o "$tmp/checksums.txt" "$base/checksums.txt"
	(cd "$tmp" && grep " $file\$" checksums.txt | sha256sum -c -)
	tar -xzf "$tmp/$file" -C "$tmp" nfpm
	install -m 0755 "$tmp/nfpm" "$cache/bin/nfpm"
	rm -rf "$tmp"
fi
echo "$cache/bin" >>"$path_file"
"$cache/bin/nfpm" --version | grep -i 'version' | head -n 1 >&2 || true
