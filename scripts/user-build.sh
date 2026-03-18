#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

docker run --rm \
  -v "$PROJECT_DIR/user":/home/ctng/user \
  ostoo-compiler bash -c '
    cd /home/ctng/user
    make "$@"
  ' -- "$@"
