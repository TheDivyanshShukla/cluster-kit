// Cluster Kit lab-PC provisioning stick. Xiao ESP32-S3 (or ESP32-S2 / DevKit) acting as a USB keyboard.
//
// Plug it in: it types the setup line by itself. Plug it into a logged-in lab PC, unplug after ~5 s, move on.
// Pick the PC type with a button held while you plug the stick in (or tap RESET while holding it):
//   plug in, then press B (BOOT) once within ~6 s ...... Windows: Win+R, PowerShell as Administrator (UAC: accept)
//   press B twice ...................................... Ubuntu: Ctrl+Alt+T, then the download line, run with sudo
//   press nothing ...................................... Windows (default)
// Do NOT hold B while plugging in: that enters the chip's download mode. Plug in, wait for the blue light, then press.
// R is the reset line and cannot be read by the sketch; B (GPIO0) is free once the board is running.
//
// Status light (the orange user LED, GPIO21): slow blink = counting B presses (6 s); fast blink = typing;
// solid 2 s = done, safe to unplug; off = idle. The red glow on the board is its power LED, not ours.
//
// Board: ESP32-S3 (Xiao or DevKit) with USB Mode "USB-OTG (TinyUSB)" and CDC On Boot disabled (hid/flash.sh sets these).
// Copy config.example.h to config.h and fill it in (config.h is git-ignored: it holds the sudo password).
#include "USB.h"
#include "USBHIDKeyboard.h"
#include "config.h"

USBHIDKeyboard Keyboard;
const int MODE_PIN = 0;  // B (BOOT) button on the Xiao: 1 press or none = Windows (default), 2 presses = Ubuntu
const char *URL_SH = "https://raw.githubusercontent.com/TheDivyanshShukla/cluster-kit/main/scripts/provision.sh";
const char *URL_PS = "https://raw.githubusercontent.com/TheDivyanshShukla/cluster-kit/main/scripts/provision.ps1";

// Status light. Boards with an RGB LED get the colour; a plain LED (Xiao: GPIO21) lights for any colour.
void led(uint8_t r, uint8_t g, uint8_t b) {
#ifdef RGB_BUILTIN
  neopixelWrite(RGB_BUILTIN, r, g, b);
#elif defined(LED_BUILTIN)
  pinMode(LED_BUILTIN, OUTPUT);
  digitalWrite(LED_BUILTIN, (r || g || b) ? HIGH : LOW);
#endif
}

// Types slowly with each key held down for a while: quick press/release pairs got keys dropped, doubled and
// changed on the PC ("curlo", "thtps", a stray space in the URL). About 40 ms per character; the line is ~400 characters.
void typeText(const String &s) {
  for (size_t i = 0; i < s.length(); i++) {
    led(i & 1, 0, 0);  // fast blink while typing
    Keyboard.press(s[i]);
    delay(25);
    Keyboard.releaseAll();
    delay(20);
  }
  led(0, 0, 0);
}

void setup() {
  pinMode(MODE_PIN, INPUT_PULLUP);
  USB.productName("CK Provision");  // hid/flash.sh looks for this name on the USB bus
  USB.manufacturerName("Cluster Kit");
  Keyboard.begin();
  USB.begin();
  // count B presses while the PC enumerates the keyboard; the LED blinks slowly during this window
  int presses = 0;
  bool last = HIGH;
  uint32_t t0 = millis();
  while (millis() - t0 < 6000) {
    bool now = digitalRead(MODE_PIN);
    if (last == HIGH && now == LOW) {
      presses++;
      delay(60);  // debounce
    }
    last = now;
    led((millis() / 500) & 1, 0, 0);
    delay(5);
  }
  led(0, 0, 0);
  bool windows = presses != 2;  // default (no press) and 1 press: Windows; 2 presses: Ubuntu

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
    delay(1800);  // the Run box must be open before the text goes in
    String cmd = String("powershell -NoProfile -ExecutionPolicy Bypass -c \"Start-Process powershell -Verb RunAs -ArgumentList '-NoExit -ExecutionPolicy Bypass -Command iex (irm ") + URL_PS + ")'\"";
    led(1, 0, 0);
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
    led(1, 0, 0);  // amber: typing
    delay(3500);     // wait for the terminal window

    // leading space: keeps the line out of the shell history (Ubuntu default HISTCONTROL=ignoreboth).
    // The script goes into a private mktemp file (other users cannot swap it before sudo runs it); curl, else wget.
    String cmd = String(" f=$(mktemp) && (curl -fsSL ") + URL_SH + " || wget -qO- " + URL_SH + ") > $f && [ -s $f ] && echo '" PASS "' | sudo -S -p '' env CK_ADMIN_KEY='" ADMIN_KEY "'" HEAD_ENV " bash $f";
    typeText(cmd);
    Keyboard.write(KEY_RETURN);
  }
  delay(300);
  led(1, 0, 0);  // solid 2 s: typed, safe to unplug
  delay(2000);
  led(0, 0, 0);
}

void loop() {
  delay(1000);  // nothing more to do: it typed once after power-up; plug it again to run again
}
