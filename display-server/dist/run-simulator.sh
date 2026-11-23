#!/bin/sh
# The PixelArena display in a window: 60 pipes standing in a 6 m circle.
# Extra options are passed on, e.g. ./run-simulator.sh --fullscreen
cd "$(dirname "$0")"
exec ./display-server --simulate 60 --width-cm 600 --height-cm 100 --pipe-cm 5 --cylinder --view 3d "$@"
