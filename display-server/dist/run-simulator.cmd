@echo off
rem The PixelArena display in a window: 60 pipes standing in a 6 m circle.
rem Extra options are passed on, e.g. run-simulator.cmd --fullscreen
cd /d "%~dp0"
display-server.exe --simulate 60 --width-cm 600 --height-cm 100 --pipe-cm 5 --cylinder --view 3d %*
