#!/usr/bin/env bash
# Native prerequisites for building/packaging dsh-launcher on Linux (Tauri 2).
set -euo pipefail

sudo apt-get update
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev \
  libappindicator3-dev \
  librsvg2-dev \
  libgtk-3-dev \
  libxdo-dev \
  libssl-dev \
  libasound2-dev \
  build-essential \
  curl \
  wget \
  file \
  patchelf \
  xdg-utils