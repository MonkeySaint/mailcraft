#!/usr/bin/env bash
# Linux packages: a relocatable tar.gz, plus .deb and .rpm through nfpm.
#
#     packaging/linux/package.sh [--build] [--target TRIPLE] [--profile release|debug]
#                                [--formats tar,deb,rpm] [--out DIR]
#
#   --build     run `cargo build` for mailcraft and mailcraft-cli first (with the build details exported)
#   --target    Rust target triple, e.g. aarch64-unknown-linux-gnu (default: the host build in target/<profile>)
#   --profile   cargo profile (default release; `debug` is handy to try the scripts locally)
#   --formats   comma list of tar, deb, rpm (default all three)
#   --out       output folder (default dist/)
#
# Output (VERSION from Cargo.toml, ARCH x86_64 or aarch64):
#   mailcraft-VERSION-linux-ARCH.tar.gz   bin/, share/ (desktop file, metainfo, icons, doc/licences)
#   mailcraft_VERSION-1_amd64.deb / _arm64.deb
#   mailcraft-VERSION-1.x86_64.rpm / .aarch64.rpm
#
# nfpm comes from $NFPM or PATH. Without it deb/rpm are skipped with a warning, unless MC_REQUIRE_NFPM=1
# (set in CI), which makes it an error.
set -euo pipefail
# shellcheck source=packaging/env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

build=0
triple=""
profile="release"
formats="tar,deb,rpm"
out="$MC_DIST"
while [ $# -gt 0 ]; do
	case "$1" in
	--build) build=1 ;;
	--target) triple="${2:?--target needs a value}"; shift ;;
	--profile) profile="${2:?--profile needs a value}"; shift ;;
	--formats) formats="${2:?--formats needs a value}"; shift ;;
	--out) out="${2:?--out needs a value}"; shift ;;
	-h | --help) sed -n '2,22p' "$0"; exit 0 ;;
	*) mc_die "unknown option $1" ;;
	esac
	shift
done

arch="${triple%%-*}"
[ -n "$arch" ] || arch="$(uname -m)"
case "$arch" in
x86_64 | amd64) arch="x86_64"; nfpm_arch="amd64" ;;
aarch64 | arm64) arch="aarch64"; nfpm_arch="arm64" ;;
*) mc_die "unsupported architecture $arch" ;;
esac

if [ "$build" = 1 ]; then
	cargo_profile="$profile"
	[ "$profile" = "debug" ] && cargo_profile="dev"
	args=(build --locked --profile "$cargo_profile" -p mailcraft -p mailcraft-cli)
	[ -n "$triple" ] && args+=(--target "$triple")
	mc_log "cargo ${args[*]} (git $MAILCRAFT_GIT_HASH, $MAILCRAFT_BUILD_DATE)"
	(cd "$MC_ROOT" && cargo "${args[@]}")
fi

bin_dir="$(mc_bin_dir "$triple" "$profile")"
work="${CARGO_TARGET_DIR:-$MC_ROOT/target}/packaging/linux-$arch"
rm -rf "$work"
mkdir -p "$work" "$out"
out="$(cd "$out" && pwd)"

has() { case ",$formats," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }

if has tar; then
	name="mailcraft-$MC_VERSION-linux-$arch"
	"$MC_ROOT/packaging/stage-unix.sh" "$bin_dir" "$work/$name"
	cat >"$work/$name/share/doc/mailcraft/INSTALL.txt" <<EOF
MailCraft $MC_VERSION for Linux ($arch)

Unpack into a prefix, for example your home folder:

    tar -xzf $name.tar.gz -C ~/.local --strip-components=1

then run \`mailcraft\` (make sure ~/.local/bin is on PATH). The desktop file and icons under share/ make it
appear in the application menu. Remove it again by deleting the files listed by \`tar -tzf $name.tar.gz\`.
EOF
	mc_tar_gz "$out/$name.tar.gz" "$work/$name"
	mc_log "wrote $out/$name.tar.gz"
fi

if has deb || has rpm; then
	nfpm="${NFPM:-$(command -v nfpm || true)}"
	if [ -z "$nfpm" ]; then
		[ "${MC_REQUIRE_NFPM:-0}" = 1 ] && mc_die "nfpm not found (set NFPM or put it on PATH)"
		mc_log "warning: nfpm not found; skipping deb/rpm"
	else
		"$MC_ROOT/packaging/stage-unix.sh" "$bin_dir" "$work/stage/usr"
		install -m 0644 "$MC_ROOT/packaging/linux/nfpm.yaml" "$work/nfpm.yaml"
		(cd "$work" && find stage/usr -type f | LC_ALL=C sort | while read -r f; do
			printf '  - src: ./%s\n    dst: /%s\n    file_info: { owner: root, group: root }\n' "$f" "${f#stage/}"
		done) >>"$work/nfpm.yaml"
		install -m 0755 "$MC_ROOT/packaging/linux/postinst.sh" "$work/postinst.sh"
		export MC_VERSION NFPM_ARCH="$nfpm_arch" MC_MAINTAINER="$MAINTAINER" MC_HOMEPAGE="$HOMEPAGE" \
			MC_SUMMARY="$APP_SUMMARY"
		for fmt in deb rpm; do
			has "$fmt" || continue
			(cd "$work" && "$nfpm" package --config nfpm.yaml --packager "$fmt" --target "$out/")
		done
	fi
fi
