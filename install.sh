#!/bin/sh
set -eu

repo="delegence/lazyagents"
bin_name="lazyagents"
install_dir="${LAZYAGENTS_INSTALL_DIR:-$HOME/.local/bin}"
version="${LAZYAGENTS_VERSION:-latest}"
caller_dir="$(pwd -P)"

fail() {
  echo "lazyagents installer: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

need curl
need tar
need uname

case "$install_dir" in
  /*) ;;
  *) install_dir="$caller_dir/$install_dir" ;;
esac

if [ -e "$install_dir" ] && [ ! -d "$install_dir" ]; then
  fail "install destination is not a directory: $install_dir"
fi

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin) os_target="apple-darwin" ;;
  Linux) os_target="unknown-linux-gnu" ;;
  *) fail "unsupported operating system: $os" ;;
esac

case "$arch" in
  x86_64 | amd64) arch_target="x86_64" ;;
  arm64 | aarch64) arch_target="aarch64" ;;
  *) fail "unsupported CPU architecture: $arch" ;;
esac

target="$arch_target-$os_target"
asset="$bin_name-$target.tar.gz"

if [ "$version" = "latest" ]; then
  base_url="https://github.com/$repo/releases/latest/download"
else
  case "$version" in
    v*) tag="$version" ;;
    *) tag="v$version" ;;
  esac
  base_url="https://github.com/$repo/releases/download/$tag"
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT INT TERM

echo "Downloading $asset..."
curl -fsSL "$base_url/$asset" -o "$tmp_dir/$asset"
curl -fsSL "$base_url/checksums.txt" -o "$tmp_dir/checksums.txt"

cd "$tmp_dir"

if command -v sha256sum >/dev/null 2>&1; then
  grep "  $asset\$" checksums.txt | sha256sum -c -
elif command -v shasum >/dev/null 2>&1; then
  expected="$(grep "  $asset\$" checksums.txt | awk '{print $1}')"
  actual="$(shasum -a 256 "$asset" | awk '{print $1}')"
  [ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"
else
  echo "lazyagents installer: sha256sum or shasum not found; skipping checksum verification" >&2
fi

tar -xzf "$asset"
mkdir -p "$install_dir" || fail "cannot create install destination: $install_dir"
cp "$bin_name-$target/$bin_name" "$install_dir/$bin_name" || fail "cannot copy binary to $install_dir"
chmod 755 "$install_dir/$bin_name" || fail "cannot make installed binary executable"

echo "Installed $bin_name to $install_dir/$bin_name"

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *)
    echo "Add $install_dir to your PATH to run $bin_name from any directory."
    ;;
esac
