#!/bin/sh
set -eu

repo="delegence/lazyagents"
bin_name="lazyagents"
version="${LAZYAGENTS_VERSION:-latest}"
caller_dir="$(pwd -P)"
tmp_dir=""
install_tmp=""

fail() {
  echo "lazyagents installer: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

need curl
need mktemp
need tar
need uname

if [ -n "${LAZYAGENTS_INSTALL_DIR:-}" ]; then
  install_dir="$LAZYAGENTS_INSTALL_DIR"
elif [ -n "${HOME:-}" ]; then
  install_dir="$HOME/.local/bin"
else
  fail "HOME is not set; set LAZYAGENTS_INSTALL_DIR"
fi

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
  Linux)
    if command -v ldd >/dev/null 2>&1; then
      libc="$(ldd --version 2>&1 || true)"
      case "$libc" in
        *musl*) fail "musl Linux is not supported; install from source" ;;
      esac
    fi
    os_target="unknown-linux-gnu"
    ;;
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
cleanup() {
  [ -z "$install_tmp" ] || rm -f "$install_tmp"
  [ -z "$tmp_dir" ] || rm -rf "$tmp_dir"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

echo "Downloading $asset..."
curl -fsSL "$base_url/$asset" -o "$tmp_dir/$asset"
curl -fsSL "$base_url/checksums.txt" -o "$tmp_dir/checksums.txt"

cd "$tmp_dir"

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$asset")"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$asset")"
else
  fail "missing required command: sha256sum or shasum"
fi
actual="${actual%% *}"

expected=""
while read -r checksum filename; do
  if [ "$filename" = "$asset" ]; then
    expected="$checksum"
    break
  fi
done < checksums.txt
[ -n "$expected" ] || fail "checksums.txt has no entry for $asset"
[ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"
echo "$asset: OK"

tar -xzf "$asset"
mkdir -p "$install_dir" || fail "cannot create install destination: $install_dir"
install_tmp="$(mktemp "$install_dir/.$bin_name.XXXXXX")" || fail "cannot create a temporary file in $install_dir"
cp "$bin_name-$target/$bin_name" "$install_tmp" || fail "cannot copy binary to $install_dir"
chmod 755 "$install_tmp" || fail "cannot make installed binary executable"
mv -f "$install_tmp" "$install_dir/$bin_name" || fail "cannot replace binary in $install_dir"
install_tmp=""

echo "Installed $bin_name to $install_dir/$bin_name"

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *)
    echo "Add $install_dir to your PATH to run $bin_name from any directory."
    ;;
esac
