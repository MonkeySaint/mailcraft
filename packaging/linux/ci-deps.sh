#!/usr/bin/env bash
# Installs what the Linux release job needs inside the Debian (rust:*-bookworm) container.
#
#     packaging/linux/ci-deps.sh [TARGET-TRIPLE]
#
# - apt: packaging tools (zsync, desktop-file-utils, appstream, file, zip, jq) and, when TRIPLE is for a
#   different architecture than the container, the GNU cross compiler for it.
# - nfpm NFPM_VERSION (MIT) from its GitHub release, checked against the release's checksums file and
#   cached in $MC_TOOL_CACHE.
# - With a CI env file ($GITHUB_ENV / $GITHUB_ENV), the cross linker settings for cargo and the cc crate,
#   and the cache folder on PATH ($GITHUB_PATH / $GITHUB_PATH).
set -euo pipefail
# shellcheck source=packaging/env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

triple="${1:-}"
nfpm_version="${NFPM_VERSION:-2.43.0}"
cache="${MC_TOOL_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/mailcraft-packaging}"
env_file="${GITHUB_ENV:-${GITHUB_ENV:-/dev/null}}"
path_file="${GITHUB_PATH:-${GITHUB_PATH:-/dev/null}}"

host="$(uname -m)"
want="${triple%%-*}"
[ -n "$want" ] || want="$host"

pkgs=(zsync desktop-file-utils appstream file zip jq curl ca-certificates
	libxkbcommon-dev libwayland-dev libgl1-mesa-dev)
cross=""
if [ "$want" != "$host" ]; then
	case "$want" in
	aarch64) cross="aarch64-linux-gnu"; pkgs+=(gcc-aarch64-linux-gnu libc6-dev-arm64-cross) ;;
	x86_64) cross="x86_64-linux-gnu"; pkgs+=(gcc-x86-64-linux-gnu libc6-dev-amd64-cross) ;;
	*) mc_die "no cross toolchain known for $want" ;;
	esac
fi

export DEBIAN_FRONTEND=noninteractive
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
