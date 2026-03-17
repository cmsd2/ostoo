#!/bin/bash

docker run -it --rm \
  -v /Volumes/crosstool-ng/x-tools:/home/ctng/x-tools \
  -v /Volumes/crosstool-ng/src:/home/ctng/src \
  ctng bash -c "cd /home/ctng/crosstool && ct-ng build"
