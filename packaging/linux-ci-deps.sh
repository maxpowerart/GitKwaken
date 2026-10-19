#!/usr/bin/env bash
# Installs what packaging/build-linux.sh and scripts/linux-smoke.sh need on an
# Ubuntu runner (ci.yml and release.yml): headers for the winit/wgpu/rfd build,
# ImageMagick and pwsh for packaging, Xvfb + Mesa (llvmpipe GL/EGL, lavapipe
# Vulkan) + xdotool for the headless launch test.
set -euo pipefail
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  libxkbcommon-dev libxkbcommon-x11-0 libwayland-dev libx11-dev libxcursor1 libxi6 libxrandr2 \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgtk-3-dev libssl-dev \
  imagemagick fonts-dejavu-core \
  xvfb xauth xdotool python3 \
  libegl1 libgl1 libgl1-mesa-dri libvulkan1 mesa-vulkan-drivers
command -v pwsh >/dev/null || { echo "pwsh missing (preinstalled on GitHub's Ubuntu images)" >&2; exit 1; }
