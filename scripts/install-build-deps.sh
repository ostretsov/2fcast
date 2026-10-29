#!/bin/sh
# Packages needed to *build* 2fcast on Ubuntu 22.04+ (plus Rust from rustup.rs).
set -e
sudo apt-get install -y build-essential pkg-config \
  libgtk-4-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev liborc-0.4-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly gstreamer1.0-libav gstreamer1.0-x gstreamer1.0-pulseaudio \
  pulseaudio-utils
