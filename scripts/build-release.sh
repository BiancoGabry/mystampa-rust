#!/usr/bin/env bash
# Build the desktop executables into dist/:
#   dist/mystampa-linux-x86_64          Linux (glibc >= 2.36: Debian 12, Ubuntu 23.04+)
#   dist/MyStampa-x86_64.AppImage       the same, packaged as an AppImage
#   dist/mystampa-windows-x86_64.exe    Windows 10/11
# Both open the MyStampa window; `--console` runs them in a terminal.
# Only Docker is needed: the toolchains live in a throwaway container.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p dist

docker run --rm \
  -v "$PWD":/src -w /src \
  -v mystampa-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/src/target/release-build \
  -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  -e SKIP_APPIMAGE="${SKIP_APPIMAGE:-}" \
  rust:1-slim-bookworm bash -euc '
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends gcc-mingw-w64-x86-64 binutils-mingw-w64-x86-64 curl ca-certificates file >/dev/null
    rustup target add x86_64-pc-windows-gnu >/dev/null
    export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
    cargo build --release --locked --target x86_64-unknown-linux-gnu
    cargo build --release --locked --target x86_64-pc-windows-gnu
    cp target/release-build/x86_64-unknown-linux-gnu/release/mystampa dist/mystampa-linux-x86_64
    cp target/release-build/x86_64-pc-windows-gnu/release/mystampa.exe dist/mystampa-windows-x86_64.exe

    if [ -z "$SKIP_APPIMAGE" ]; then
      # AppImage: AppDir with the binary, AppRun, desktop file and icon.
      # appimagetool comes from the official AppImage project on GitHub.
      TOOLS=target/release-build/tools; mkdir -p "$TOOLS"
      if [ ! -x "$TOOLS/appimagetool" ]; then
        curl -fsSL -o "$TOOLS/appimagetool" \
          https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
        chmod +x "$TOOLS/appimagetool"
      fi
      APPDIR=target/release-build/MyStampa.AppDir
      rm -rf "$APPDIR"; mkdir -p "$APPDIR/usr/bin"
      cp dist/mystampa-linux-x86_64 "$APPDIR/usr/bin/mystampa"
      cp packaging/appimage/AppRun packaging/appimage/mystampa.desktop packaging/appimage/mystampa.png "$APPDIR/"
      ln -s mystampa.png "$APPDIR/.DirIcon"
      # No FUSE in a container: run the tool extracted
      APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$TOOLS/appimagetool" --no-appstream "$APPDIR" dist/MyStampa-x86_64.AppImage
    fi
    chown -R "$HOST_UID:$HOST_GID" dist target/release-build
  '
ls -lh dist
