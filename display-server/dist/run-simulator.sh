#!/bin/sh
# The PixelArena display in a window: 60 pipes standing in a circle 3 m across.
# Extra options are passed on, e.g. ./run-simulator.sh --fullscreen
cd "$(dirname "$0")"
exec ./display-server --simulate 60 --width-cm 942 --height-cm 100 --pipe-cm 5 --cylinder --view 3d "$@"
