#!/usr/bin/env bash
# System libraries the editor (eframe/winit/wgpu) links against on Ubuntu.
set -euo pipefail
sudo apt-get update -qq
sudo apt-get install -y -qq --no-install-recommends \
  pkg-config libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libx11-dev libxcursor-dev libxrandr-dev libxi-dev libxcb1-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libvulkan-dev libdbus-1-dev
