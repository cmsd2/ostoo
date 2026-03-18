#!/bin/bash
set -e

cd "$(dirname "$0")"

# Build the ctng stage (has crosstool-ng for menuconfig)
docker build --target ctng -t ostoo-ctng .

# Run interactively, mounting the crosstool dir so .config changes come back to the host
docker run -it --rm \
  -v "$(pwd):/home/ctng/crosstool" \
  ostoo-ctng bash
