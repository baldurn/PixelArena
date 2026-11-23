#!/usr/bin/env bash
# Builds the display server and the demos and packs them with the run
# scripts, for people to run the simulator on their own computer:
#   dist/package.sh NAME   ->   dist/out/PixelArena-NAME.tar.gz (.zip on Windows)
# On macOS with rustup it builds for Apple silicon and Intel and joins them.
# The release workflow (.github/workflows/release.yml) runs this.
set -euo pipefail
cd "$(dirname "$0")/.."
NAME=${1:?usage: dist/package.sh NAME}
DEMOS=$(for f in demos/src/bin/*.rs; do basename "$f" .rs; done)
DIR=PixelArena-$NAME
OUT=dist/out/$DIR
rm -rf "$OUT" "$OUT".tar.gz "$OUT".zip
mkdir -p "$OUT/demos"

build() { # build [target]
  cargo build --release --locked ${1:+--target "$1"}
  (cd demos && cargo build --release --locked ${1:+--target "$1"})
}

case "$(uname -s)" in
  Darwin)
    if command -v rustup >/dev/null; then
      targets="aarch64-apple-darwin x86_64-apple-darwin"
      rustup target add $targets
      for t in $targets; do build "$t"; done
      lipo -create -output "$OUT/display-server" target/{aarch64,x86_64}-apple-darwin/release/display-server
      for d in $DEMOS; do
        lipo -create -output "$OUT/demos/$d" demos/target/{aarch64,x86_64}-apple-darwin/release/"$d"
      done
    else
      build  # this Mac's architecture only
      cp target/release/display-server "$OUT/"
      for d in $DEMOS; do cp demos/target/release/"$d" "$OUT/demos/"; done
    fi
    ;;
  MINGW* | MSYS* | CYGWIN*)
    build
    cp target/release/display-server.exe "$OUT/"
    for d in $DEMOS; do cp demos/target/release/"$d".exe "$OUT/demos/"; done
    ;;
  *)
    build
    cp target/release/display-server "$OUT/"
    for d in $DEMOS; do cp demos/target/release/"$d" "$OUT/demos/"; done
    ;;
esac

cp dist/README.txt "$OUT/"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
    cp dist/run-simulator.cmd "$OUT/"
    (cd dist/out && 7z a -tzip "$DIR.zip" "$DIR" >/dev/null)
    echo "dist/out/$DIR.zip"
    ;;
  *)
    cp dist/run-simulator.sh demos/cycle.sh "$OUT/"
    mv "$OUT/cycle.sh" "$OUT/demos/"
    (cd dist/out && tar czf "$DIR.tar.gz" "$DIR")
    echo "dist/out/$DIR.tar.gz"
    ;;
esac
