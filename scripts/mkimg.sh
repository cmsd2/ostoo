#!/bin/bash

hdiutil create -size 64m -fs ExFAT -layout NONE -volname MyVolume disk.img
