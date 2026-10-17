#!/usr/bin/env bash
# NixOS only. gpui builds against fontconfig/freetype/xkbcommon/xcb .pc files
# that a plain NixOS profile doesn't expose, and links libs that are not on the
# loader path (wayland and vulkan are system-wide, the rest are not).
#
#   scripts/dev.sh cargo run -p smash-gpui
#   scripts/dev.sh cargo test
set -euo pipefail

dev="$(nix build --no-link --print-out-paths \
  nixpkgs#fontconfig^dev \
  nixpkgs#freetype^dev \
  nixpkgs#libxkbcommon^dev \
  nixpkgs#xorg.libxcb^dev \
  nixpkgs#xorg.libX11^dev \
  nixpkgs#xorg.libXcursor^dev \
  nixpkgs#xorg.libXrandr^dev \
  nixpkgs#xorg.libXi^dev \
  | sed 's|$|/lib/pkgconfig|' | paste -sd:)"

libs="$(nix build --no-link --print-out-paths \
  nixpkgs#fontconfig^lib \
  nixpkgs#freetype^out \
  nixpkgs#libxkbcommon^out \
  nixpkgs#xorg.libxcb^out \
  nixpkgs#xorg.libX11^out \
  nixpkgs#xorg.libXcursor^out \
  nixpkgs#xorg.libXrandr^out \
  nixpkgs#xorg.libXi^out \
  nixpkgs#wayland^out \
  nixpkgs#vulkan-loader^out \
  | sed 's|$|/lib|' | paste -sd:)"

export PKG_CONFIG_PATH="$dev:${PKG_CONFIG_PATH:-}"
# rustc forwards LIBRARY_PATH to the linker as -L, which is how crates that
# hardcode `-lxcb`/`-lxkbcommon` without a search path find them here.
export LIBRARY_PATH="$libs:${LIBRARY_PATH:-}"
export LD_LIBRARY_PATH="$libs:${LD_LIBRARY_PATH:-}"

exec "$@"
