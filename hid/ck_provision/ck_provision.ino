// Cluster Kit lab-PC provisioning stick. ESP32-S3 or ESP32-S2 (native USB) acting as a USB keyboard.
// Plug it into an Ubuntu PC that is logged in to its desktop (US keyboard layout): it opens a terminal and types one line
// that downloads scripts/provision.sh from the repo and runs it with sudo. Typing takes about 3 s, so unplug right after
// and move to the next PC; the script keeps running in that terminal and prints DONE when finished.
//
// SAFETY: it types ONLY while pin GPIO4 is tied to GND (leave a jumper wire there in the lab). Without the jumper the LED
// blinks red and nothing is typed, so flashing it or plugging it into your own computer is harmless.
//
// Arduino IDE / arduino-cli: board "ESP32S3 Dev Module" (or ESP32S2), USB Mode = "USB-OTG (TinyUSB)", USB CDC On Boot = Disabled.
// Copy config.example.h to config.h and fill it in (config.h is git-ignored: it holds the sudo password).
#include "USB.h"
#include "USBHIDKeyboard.h"
#include "config.h"

USBHIDKeyboard Keyboard;
const int ARM_PIN = 4;  // jumper GPIO4 to GND to arm
const int OS_PIN = 5;   // the stick cannot see the OS: jumper GPIO5 to GND for Windows, leave open for Ubuntu
const char *URL_SH = "https://raw.githubusercontent.com/TheDivyanshShukla/cluster-kit/main/scripts/provision.sh";
const char *URL_PS = "https://raw.githubusercontent.com/TheDivyanshShukla/cluster-kit/main/scripts/provision.ps1";

void led(uint8_t r, uint8_t g, uint8_t b) {
#ifdef RGB_BUILTIN
  neopixelWrite(RGB_BUILTIN, r, g, b);
#endif
}

void typeText(const String &s) {
  for (size_t i = 0; i < s.length(); i++) {
    Keyboard.write(s[i]);
    delay(6);  // some hosts drop keys when typed faster
  }
}

bool armed = false;

void setup() {
  pinMode(ARM_PIN, INPUT_PULLUP);
  pinMode(OS_PIN, INPUT_PULLUP);
  USB.productName("CK Provision");  // hid/flash.sh looks for this name on the USB bus
  USB.manufacturerName("Cluster Kit");
  Keyboard.begin();
  USB.begin();
  led(0, 0, 40);  // blue: starting
  delay(1500);
  armed = digitalRead(ARM_PIN) == LOW;
  if (!armed) return;  // loop() blinks red

  delay(4500);  // the PC enumerates the keyboard
  bool windows = digitalRead(OS_PIN) == LOW;
  Keyboard.press(KEY_LEFT_SHIFT);  // wake a blanked screen
  delay(60);
  Keyboard.releaseAll();
  delay(800);
  if (windows) {
    // Win+R, then one PowerShell line that re-launches itself elevated (UAC asks once; the default button is Yes)
    Keyboard.press(KEY_LEFT_GUI);
    Keyboard.press('r');
    delay(120);
    Keyboard.releaseAll();
    delay(1200);
    String cmd = String("powershell -NoProfile -ExecutionPolicy Bypass -c \"Start-Process powershell -Verb RunAs -ArgumentList '-NoExit -ExecutionPolicy Bypass -Command iex (irm ") + URL_PS + ")'\"";
    led(40, 25, 0);
    typeText(cmd);
    Keyboard.write(KEY_RETURN);
    delay(4000);
    Keyboard.write(KEY_RETURN);  // UAC: accept
  } else {
    Keyboard.press(KEY_LEFT_CTRL);  // Ctrl+Alt+T: GNOME terminal
    Keyboard.press(KEY_LEFT_ALT);
    Keyboard.press('t');
    delay(120);
    Keyboard.releaseAll();
    led(40, 25, 0);  // amber: typing
    delay(3500);     // wait for the terminal window

    // leading space: keeps the line out of the shell history (Ubuntu default HISTCONTROL=ignoreboth).
    // The script goes into a private mktemp file (other users cannot swap it before sudo runs it); curl, else wget.
    String cmd = String(" f=$(mktemp) && (curl -fsSL ") + URL_SH + " || wget -qO- " + URL_SH + ") > $f && [ -s $f ] && echo '" PASS "' | sudo -S -p '' env CK_ADMIN_KEY='" ADMIN_KEY "'" HEAD_ENV " bash $f";
    typeText(cmd);
    Keyboard.write(KEY_RETURN);
  }
  delay(300);
  led(0, 40, 0);  // green: typed, safe to unplug
}

void loop() {
  if (!armed) {
    led(40, 0, 0);
    delay(300);
    led(0, 0, 0);
    delay(1200);
  }
  delay(100);
}
