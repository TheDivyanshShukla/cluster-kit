#!/usr/bin/env bash
# Flash the Cluster Kit HID stick. Any ESP32-S3 or ESP32-S2 board, any USB port, from a Mac or a Linux PC:
#   hid/flash.sh [--os ubuntu|windows|auto] [serial-port]
# CK_PROVISION_BRANCH selects the GitHub branch used by the Ubuntu bootstrap (default: main).
# It finds the board, identifies the chip, builds ck_provision for that chip (the first build downloads the ESP32 core,
# a few hundred MB), writes the whole flash image with esptool and checks the board comes back as a USB keyboard.
# Needs: arduino-cli, and uv (or esptool on PATH).
# If the board already runs the HID firmware no serial port shows up: hold BOOT, tap RESET (or hold BOOT while plugging
# it in), then run this again.
set -euo pipefail
cd "$(dirname "$0")"
SKETCH=ck_provision
PORT=""
OS=auto
while [ "$#" -gt 0 ]; do
  case "$1" in
    --os) [ "$#" -ge 2 ] || { echo "--os needs ubuntu, windows, or auto" >&2; exit 1; }; OS=$2; shift 2 ;;
    -*) echo "Unknown option: $1" >&2; exit 1 ;;
    *) [ -z "$PORT" ] || { echo "Only one serial port is allowed" >&2; exit 1; }; PORT=$1; shift ;;
  esac
done
case $OS in ubuntu) TARGET=1 ;; windows) TARGET=2 ;; auto) TARGET=0 ;; *) echo "Invalid OS: $OS" >&2; exit 1 ;; esac
BRANCH=${CK_PROVISION_BRANCH:-main}
case $BRANCH in *[!a-zA-Z0-9_./-]*|'') echo "Invalid provisioning branch" >&2; exit 1 ;; esac
say() { printf '\n== %s\n' "$*"; }
die() { printf '!! %s\n' "$*" >&2; exit 1; }

command -v arduino-cli >/dev/null 2>&1 || die "arduino-cli missing: brew install arduino-cli (Mac) or https://arduino.github.io/arduino-cli/latest/installation/"
if command -v esptool >/dev/null 2>&1; then ESPTOOL="esptool"
elif command -v uvx >/dev/null 2>&1; then ESPTOOL="uvx --quiet esptool"
else die "esptool missing: install uv (https://docs.astral.sh/uv/) or run: pip install esptool"; fi
# esptool 5 spells commands with hyphens, esptool 4 with underscores
if $ESPTOOL --help 2>&1 | grep 'write-flash' >/dev/null; then CHIPID=chip-id; WRITE=write-flash; AFTER=no-reset; else CHIPID=chip_id; WRITE=write_flash; AFTER=no_reset; fi
[ -f "$SKETCH/config.h" ] || die "$SKETCH/config.h is missing: copy $SKETCH/config.example.h to $SKETCH/config.h and fill it in"

ports() { ls /dev/cu.usbmodem* /dev/cu.usbserial* /dev/cu.wchusbserial* /dev/cu.SLAB_USBtoUART* /dev/ttyACM* /dev/ttyUSB* 2>/dev/null || true; }
chip_of() {  # $1 = port; prints esp32s3 | esp32s2 | the raw chip name; returns 1 when the board does not answer
  local out name
  out=$($ESPTOOL --port "$1" --after "$AFTER" $CHIPID 2>&1) || return 1
  name=$(printf '%s\n' "$out" | grep -oE 'ESP32(-[A-Z0-9]+)*' | head -1)
  case $name in *S3*) echo esp32s3 ;; *S2*) echo esp32s2 ;; *) echo "${name:-unknown}" ;; esac
}

say "looking for the board"
CHIP=""
if [ -n "$PORT" ]; then
  CHIP=$(chip_of "$PORT") || die "no answer on $PORT (hold BOOT, tap RESET, try again)"
else
  for p in $(ports); do
    c=$(chip_of "$p") || continue
    case $c in esp32s3|esp32s2) PORT=$p; CHIP=$c; break ;; *) echo "   $p: $c cannot act as a USB keyboard, skipped" ;; esac
  done
fi
[ -n "$PORT" ] || die "no ESP32-S3/S2 found. Plug it in; if it already runs the HID firmware hold BOOT while plugging it in. Serial ports seen: $(ports | tr '\n' ' ')"
case $CHIP in esp32s3|esp32s2) ;; *) die "$CHIP has no USB device mode, so it cannot be a keyboard. Use an ESP32-S3 or ESP32-S2 board." ;; esac
echo "   $CHIP on $PORT"

case $CHIP in
  esp32s3) FQBN="esp32:esp32:esp32s3:USBMode=default,CDCOnBoot=default,FlashMode=dio,FlashSize=4M,PartitionScheme=default,PSRAM=disabled" ;;
  esp32s2) FQBN="esp32:esp32:esp32s2:CDCOnBoot=default,FlashMode=dio,FlashSize=4M,PartitionScheme=default,PSRAM=disabled" ;;
esac

say "building for $CHIP (TinyUSB keyboard, 4 MB image that boots on any larger flash)"
arduino-cli core list 2>/dev/null | grep -q '^esp32:esp32' || {
  arduino-cli config get board_manager.additional_urls 2>/dev/null | grep -q espressif.github.io ||
    arduino-cli config add board_manager.additional_urls https://espressif.github.io/arduino-esp32/package_esp32_index.json
  arduino-cli core update-index && arduino-cli core install esp32:esp32
}
# Apple Silicon without Rosetta cannot run arduino-cli's x86 ctags ("bad CPU type"). The sketch defines everything before
# use, so ctags has nothing to add: swap in a no-op (the original stays as ctags.x86).
CT=$(ls "$HOME"/Library/Arduino15/packages/builtin/tools/ctags/*/ctags 2>/dev/null | head -1 || true)
if [ -n "$CT" ] && ! "$CT" --version >/dev/null 2>&1; then
  mv "$CT" "$CT.x86" && printf '#!/bin/sh\nexit 0\n' > "$CT" && chmod +x "$CT" && echo "   ctags cannot run here: using a no-op"
fi
OUT=build/$OS/$CHIP
BIN=$OUT/$SKETCH.ino.merged.bin
rm -f "$BIN"
arduino-cli compile --fqbn "$FQBN" --build-property "compiler.cpp.extra_flags=-DCK_TARGET_OS=$TARGET -DCK_BRANCH_NAME=$BRANCH" --output-dir "$OUT" "$SKETCH"
[ -s "$BIN" ] || die "build produced no $BIN (see the compiler output above)"

say "flashing $(wc -c < "$BIN" | tr -d ' ') bytes"
ok=""
for baud in 921600 460800 115200; do
  if $ESPTOOL --chip "$CHIP" --port "$PORT" --baud $baud --after "$AFTER" $WRITE 0x0 "$BIN"; then ok=1; break; fi
  echo "   $baud baud failed, trying slower"
  for _ in 1 2 3 4 5 6 7 8; do [ -e "$PORT" ] && break; sleep 1; done  # the port vanishes while the board re-enumerates
done
[ -n "$ok" ] || die "flashing failed at every speed: hold BOOT, tap RESET, run again"

say "flash verified; board left in download mode to avoid typing into the flashing computer"

echo
case $PORT in
  *usbserial*|*wchusbserial*|*SLAB*|*ttyUSB*) echo "NOTE: you flashed through the UART bridge port. Plug the PC into the OTHER USB-C port (the one labelled USB), that one is the keyboard." ;;
esac
echo "Done: $OS firmware, Ubuntu provisioning branch $BRANCH. Unplug and move it to the target PC; wait until the typing light finishes before removing it."
echo "To flash again: hold BOOT, tap RESET (or hold BOOT while plugging in), run hid/flash.sh."
