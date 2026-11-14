// Cluster Kit lab-PC provisioning stick. Xiao ESP32-S3 (or ESP32-S2 / DevKit) acting as a USB keyboard.
//
// Plug it in: it types the setup line by itself. Plug it into a logged-in lab PC, unplug after the typing light finishes (USB-paced typing; wait for the done light), move on.
// Pick the PC type with a button held while you plug the stick in (or tap RESET while holding it):
//   plug in, then press B (BOOT) once within ~6 s ...... Windows: Win+R, PowerShell as Administrator (UAC: accept)
//   press B twice ...................................... Ubuntu: Ctrl+Alt+T, then the download line, run with sudo
//   press nothing ...................................... Windows (default)
// Do NOT hold B while plugging in: that enters the chip's download mode. Plug in, wait for the blue light, then press.
// R is the reset line and cannot be read by the sketch; B (GPIO0) is free once the board is running.
//
// Status light (the orange user LED, GPIO21): slow blink = counting B presses (2.5 s); fast blink = typing;
// solid 2 s = done, safe to unplug; off = idle. The red glow on the board is its power LED, not ours.
//
// Board: ESP32-S3 (Xiao or DevKit) with USB Mode "USB-OTG (TinyUSB)" and CDC On Boot disabled (hid/flash.sh sets these).
// Copy config.example.h to config.h and fill it in (config.h is git-ignored: it holds the sudo password).
#include "USB.h"
#include "USBHIDKeyboard.h"
#include "config.h"
#include "ubuntu_bootstrap.h"
#include "checked_command.h"

#ifndef CK_TARGET_OS
#define CK_TARGET_OS 0  // 0: button selection, 1: Ubuntu only, 2: Windows only
#endif
#ifndef CK_BRANCH_NAME
#define CK_BRANCH_NAME main
#endif
#define CK_STRING_(value) #value
#define CK_STRING(value) CK_STRING_(value)
#define CK_PROVISION_BRANCH CK_STRING(CK_BRANCH_NAME)

USBHIDKeyboard Keyboard;
const int MODE_PIN = 0;  // B (BOOT) button on the Xiao: 1 press or none = Windows (default), 2 presses = Ubuntu
const char *URL_SH = "https://raw.githubusercontent.com/TheDivyanshShukla/cluster-kit/" CK_PROVISION_BRANCH "/scripts/provision.sh";
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

// Send at the USB endpoint rate: the installed Arduino USBHID::SendReport waits for
// transfer completion and the HID endpoint polls every 1 ms. Keep separate press
// and release reports so identical consecutive characters register independently.
// Ubuntu terminals can need extra settling time for both key states.
void typeText(const String &s, unsigned keyDelayMs = 0) {
  for (size_t i = 0; i < s.length(); i++) {
    led(i & 1, 0, 0);  // fast blink while typing
    Keyboard.press(s[i]);
    if (keyDelayMs) delay(keyDelayMs);
    Keyboard.releaseAll();
    if (keyDelayMs) delay(keyDelayMs);
  }
  led(0, 0, 0);
}

// Quote configuration values as one shell argument, including embedded apostrophes.
String shellQuote(const char *value) {
  String out = "'";
  while (*value) {
    if (*value == '\'') out += "'\"'\"'";
    else out += *value;
    value++;
  }
  return out + "'";
}

void setup() {
  pinMode(MODE_PIN, INPUT_PULLUP);
  #if CK_TARGET_OS == 1
  USB.productName("CK Provision Ubuntu");
#else
  USB.productName("CK Provision");
#endif  // hid/flash.sh looks for this name on the USB bus
  USB.manufacturerName("Cluster Kit");
  Keyboard.begin();
  USB.begin();
  // count B presses while the PC enumerates the keyboard; the LED blinks slowly during this window
  int presses = 0;
  bool last = HIGH;
  uint32_t t0 = millis();
  while (millis() - t0 < 2500) {
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
  bool windows = CK_TARGET_OS == 2 || (CK_TARGET_OS == 0 && presses != 2);  // default (no press) and 1 press: Windows; 2 presses: Ubuntu

  Keyboard.press(KEY_LEFT_SHIFT);  // wake a blanked screen
  delay(60);
  Keyboard.releaseAll();
  delay(800);
  if (windows) {
    // Win+R opens a visible Administrator PowerShell; the person approves UAC.
    // Keep the command below the classic Run limit and the initial console open.
    // Use RemoteSigned for this process only; enforced Group Policy still takes precedence.
    Keyboard.press(KEY_LEFT_GUI);
    Keyboard.press('r');
    delay(120);
    Keyboard.releaseAll();
    delay(1800);  // the Run box must be open before the text goes in
    String cmd = String("powershell -NoExit -NoProfile -ExecutionPolicy RemoteSigned -Command \"$p=$env:TEMP+'/ck.ps1';Invoke-WebRequest -UseBasicParsing ") + URL_PS + " -OutFile $p -ErrorAction Stop;& $p\"";
    led(1, 0, 0);
    typeText(cmd);
    // Run's Ctrl+Shift+Enter requests Administrator rights; UAC is approved by the person.
    Keyboard.press(KEY_LEFT_CTRL);
    Keyboard.press(KEY_LEFT_SHIFT);
    Keyboard.press(KEY_RETURN);
    delay(120);
    Keyboard.releaseAll();
  } else {
    Keyboard.press(KEY_LEFT_CTRL);  // Ctrl+Alt+T: GNOME terminal
    Keyboard.press(KEY_LEFT_ALT);
    Keyboard.press('t');
    delay(120);
    Keyboard.releaseAll();
    led(1, 0, 0);  // amber: typing
    delay(3500);     // wait for the terminal window

    // Authenticate first, then download with system tools outside the Conda PATH.
    // Leading space keeps this command out of Ubuntu's default shell history.
    String cmd = String(" printf '%s\\n' ") + shellQuote(PASS) + " | sudo -S -p '' /usr/bin/env PATH=/usr/sbin:/usr/bin:/sbin:/bin CK_FLEET=open CK_AUTH=off CK_REPLACE=1 CK_FOLLOW=1 CK_ROLE=worker CK_BRANCH=" + shellQuote(CK_PROVISION_BRANCH) + " CK_ADMIN_KEY=" + shellQuote(ADMIN_KEY) + HEAD_ENV + " /bin/bash -c " + shellQuote(UBUNTU_BOOTSTRAP) + " ck-bootstrap " + shellQuote(URL_SH);
    typeText(checkedCommand(cmd));
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
