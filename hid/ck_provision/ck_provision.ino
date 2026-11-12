// Cluster Kit lab-PC provisioning stick. Xiao ESP32-S3 (or ESP32-S2 / DevKit) acting as a USB keyboard.
//
// Plug it in: it types the setup line by itself. Plug it into a logged-in lab PC, unplug after ~5 s, move on.
// Pick the PC type with a button held while you plug the stick in (or tap RESET while holding it):
//   no button ............ Ubuntu (default)
//   BTN_LINUX held ....... Ubuntu: Ctrl+Alt+T, then the download line, run with sudo
//   BTN_WIN held ......... Windows: Win+R, PowerShell as Administrator (UAC: accept, default is Yes)
// Button wiring: one leg to the pin below, the other leg to any GND pad. On the Xiao: BTN_WIN = D3 (GPIO4),
// BTN_LINUX = D4 (GPIO5). Change the two numbers below if you wire them elsewhere.
//
// Status light: blue = starting, amber = typing, green = done (safe to unplug). The red glow on the board is its power LED.
//
// Board: ESP32-S3 (Xiao or DevKit) with USB Mode "USB-OTG (TinyUSB)" and CDC On Boot disabled (hid/flash.sh sets these).
// Copy config.example.h to config.h and fill it in (config.h is git-ignored: it holds the sudo password).
#include "USB.h"
#include "USBHIDKeyboard.h"
#include "config.h"

USBHIDKeyboard Keyboard;
const int BTN_WIN = 4;    // GPIO4 = D3 on the Xiao: hold while plugging in for Windows
const int BTN_LINUX = 5;  // GPIO5 = D4 on the Xiao: hold while plugging in for Ubuntu (also the default)
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

void typeText(const String &s) {
  for (size_t i = 0; i < s.length(); i++) {
    Keyboard.write(s[i]);
    delay(6);  // some hosts drop keys when typed faster
  }
}

void setup() {
  pinMode(BTN_WIN, INPUT_PULLUP);
  pinMode(BTN_LINUX, INPUT_PULLUP);
  USB.productName("CK Provision");  // hid/flash.sh looks for this name on the USB bus
  USB.manufacturerName("Cluster Kit");
  Keyboard.begin();
  USB.begin();
  led(0, 0, 40);  // blue: starting
  delay(1500);
  bool windows = digitalRead(BTN_WIN) == LOW && digitalRead(BTN_LINUX) == HIGH;  // both held (or neither): Ubuntu
  delay(3500);  // the PC enumerates the keyboard

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
  delay(1000);  // nothing more to do: it typed once after power-up; plug it again to run again
}
