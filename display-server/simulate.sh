#!/bin/bash
# The simulator as set up on the Pi: 60 pipes standing in a circle 3 m across.
# Extra options are passed on, e.g. ./simulate.sh --fullscreen
# display-simulator.service runs this at boot.
export DISPLAY=${DISPLAY:-:0}
exec ~/display-server/display-server --simulate 60 --width-cm 942 --height-cm 100 --pipe-cm 5 --cylinder --view 3d "$@"
