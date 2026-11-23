#!/bin/bash
# The simulator as set up on the Pi: 60 pipes standing in a 6 m circle.
# Extra options are passed on, e.g. ./simulate.sh --fullscreen
# display-simulator.service runs this at boot.
export DISPLAY=${DISPLAY:-:0}
exec ~/display-server/display-server --simulate 60 --width-cm 600 --height-cm 100 --pipe-cm 5 --cylinder --view 3d "$@"
