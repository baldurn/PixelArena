PixelArena simulator
====================

The PixelArena LED display in a window on your own computer: 60 pipes of
144 LEDs standing in a circle 3 m across. Games and demos connect to it exactly as
they do to the real display, so you can write and try a game here first.

1. Start the simulator
   - Windows: double-click run-simulator.cmd
   - macOS / Linux: run ./run-simulator.sh in a terminal

2. Start a game or demo, while the simulator runs
   - Windows: double-click demos\duel.exe (or any other demo)
   - macOS / Linux: ./demos/duel (or any other demo)
   - macOS / Linux: ./demos/cycle.sh shows all demos in a loop

3. Write your own game
   Open http://localhost:8080/ in a browser. That page is the complete
   protocol; hand it to an AI to have a game written.

In the simulator window
   Player 1: arrows to move, Enter to fire
   Player 2: W A S D to move, Space to fire
   F: fullscreen, Esc: leave fullscreen, V: flat / 3D view
   I: hide the address text, drag: orbit (3D), scroll: zoom, R: spin

WLED and xLights can drive the simulator too: send DDP to this computer's
address (shown in the window).

Notes
   - Sound plays through the computer's default sound output. On Linux it
     needs PipeWire (pw-cat) or ALSA (aplay); --audio none turns it off.
   - Windows may ask whether display-server may use the network: allow it
     for the private network so games and WLED can connect.
   - macOS blocks downloaded programs that are not signed. Either
     right-click display-server, choose Open and confirm, or run once in a
     terminal in this folder:  xattr -dr com.apple.quarantine .
   - display-server --help lists every option.
