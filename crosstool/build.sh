#!/bin/bash
set -e

cd "$(dirname "$0")"

# Build the slim compiler image (also builds the toolchain on first run)
docker build -t ostoo-compiler .
