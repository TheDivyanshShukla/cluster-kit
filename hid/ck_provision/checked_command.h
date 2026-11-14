#include "base64.h"
#include "mbedtls/sha256.h"

// A damaged encoded payload must fail verification before Bash can execute it.
String checkedPayload(const String &encoded, const String &digest) {
  return String(" f=$(mktemp) && printf %s '") + encoded + "' | base64 -d > \"$f\" && printf '%s  %s\\n' '" + digest + "' \"$f\" | sha256sum -c - && /bin/bash \"$f\"; rm -f \"$f\"";
}

String checkedCommand(const String &command) {
  unsigned char digest[32];
  char hex[65];
  if (mbedtls_sha256((const unsigned char *)command.c_str(), command.length(), digest, 0) != 0) {
    return " echo 'Could not checksum provisioning command'";
  }
  for (size_t i = 0; i < sizeof(digest); i++) snprintf(hex + i * 2, 3, "%02x", digest[i]);
  return checkedPayload(base64::encode(command), String(hex));
}
