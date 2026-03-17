#!/bin/bash
set -e

# Fix ownership of the volume mounts, then drop to ctng user
chown -R ctng:ctng /home/ctng/x-tools
chown -R ctng:ctng /home/ctng/src
chown -R ctng:ctng /home/ctng/crosstool

exec gosu ctng "$@"
