@echo off
rem The PixelArena display in a window: 60 pipes standing in a circle 3 m across.
rem Extra options are passed on, e.g. run-simulator.cmd --fullscreen
cd /d "%~dp0"
display-server.exe --simulate 60 --width-cm 942 --height-cm 100 --pipe-cm 5 --cylinder --view 3d %*
