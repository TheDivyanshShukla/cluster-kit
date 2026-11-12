#!/usr/bin/env bash
# Provision one Ubuntu lab PC for Cluster Kit. Idempotent and self-healing: every item is checked first, repaired when
# it is wrong, and checked again; a PC that is already fine finishes in a few seconds. The ESP32 stick in hid/ types this.
#   sudo bash provision.sh
# Settings (environment): CK_HEAD_IP    pin the head (default: workers find it by broadcast)
#                         CK_FLEET      open | secured (default open)      CK_JOIN_CODE  for a secured fleet
#                         CK_BRANCH     git branch (default main)           CK_ADMIN_KEY  ssh public key line to allow in
#                         CK_ROLE=head  also run the head here as a service  CK_AUTH  off (default): no login, no token
[ "$(id -u)" = 0 ] || exec sudo -E bash "$0" "$@"
LOG=/var/log/ck-provision.log
if [ -z "${CK_LOGGED:-}" ]; then  # run again with everything copied to the log
  [ -t 1 ] && export CK_TTY=1
  CK_LOGGED=1 bash "$0" "$@" 2>&1 | tee -a "$LOG"
  exit "${PIPESTATUS[0]}"
fi
set -u
REPO=${CK_REPO:-https://github.com/TheDivyanshShukla/cluster-kit.git}
BRANCH=${CK_BRANCH:-main}
HEAD_IP=${CK_HEAD_IP:-}
FLEET=${CK_FLEET:-open}
FAILED=0
if [ -n "${CK_TTY:-}" ]; then G=$'\e[32m'; Y=$'\e[33m'; R=$'\e[31m'; N=$'\e[0m'; else G=''; Y=''; R=''; N=''; fi
step()  { printf '\n== %s\n' "$*"; }
ok()    { printf '  %sok%s      %s\n' "$G" "$N" "$*"; }
fixed() { printf '  %sfixed%s   %s\n' "$Y" "$N" "$*"; }
bad()   { printf '  %sFAIL%s    %s\n' "$R" "$N" "$*"; FAILED=$((FAILED + 1)); }
retry() { local n=$1 i; shift; for i in $(seq 1 "$n"); do "$@" && return 0; sleep 3; done; return 1; }

# the desktop user: whoever ran sudo, else the first logged-in non-root user, else the first normal account
U=${SUDO_USER:-}
{ [ -n "$U" ] && [ "$U" != root ]; } || U=$(loginctl list-sessions --no-legend 2>/dev/null | awk '$3!="root"{print $3; exit}')
[ -n "$U" ] || U=$(awk -F: '$3>=1000 && $3<60000 && $6 ~ /^\/home\//{print $1; exit}' /etc/passwd)
[ -n "$U" ] || { echo "no desktop user found"; exit 1; }
UID_=$(id -u "$U"); UH=$(getent passwd "$U" | cut -d: -f6); DIR=$UH/Desktop/cluster-kit; BIN=$DIR/rust/target/release/ck
as_user() { sudo -u "$U" -H env "PATH=$UH/.cargo/bin:$PATH" "XDG_RUNTIME_DIR=/run/user/$UID_" "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$UID_/bus" "$@"; }
export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=a NEEDRESTART_SUSPEND=1
echo "Cluster Kit provisioning $(date '+%F %T') on $(hostname) for user $U"

step "clock and network"
synced() { timedatectl show -p NTPSynchronized --value 2>/dev/null; }
if [ "$(synced)" = no ]; then  # a wrong clock makes every https download fail
  timedatectl set-ntp true 2>/dev/null; systemctl restart systemd-timesyncd 2>/dev/null
  for _ in $(seq 1 15); do [ "$(synced)" = yes ] && break; sleep 1; done
  [ "$(synced)" = yes ] && fixed "clock was not synchronised; now $(date '+%F %T')" || bad "clock is not synchronised (https downloads may fail)"
else
  ok "clock $(date '+%F %T')"
fi
online() { timeout 8 bash -c 'exec 3<>/dev/tcp/github.com/443' 2>/dev/null; }
ONLINE=0
if retry 3 online; then
  ONLINE=1; ok "github.com reachable"
else
  if ! getent hosts github.com >/dev/null 2>&1; then  # name lookup is what is broken: add public DNS servers
    mkdir -p /etc/systemd/resolved.conf.d
    printf '[Resolve]\nDNS=1.1.1.1 8.8.8.8\n' > /etc/systemd/resolved.conf.d/ck-dns.conf
    systemctl restart systemd-resolved 2>/dev/null; sleep 2
    retry 3 online && { ONLINE=1; fixed "DNS did not resolve; added public DNS servers"; }
  fi
  [ $ONLINE = 1 ] || bad "github.com unreachable: downloads skipped, the PC keeps what it already has"
fi

step "no sleep, no screen lock, no timeouts"
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target >/dev/null 2>&1
mkdir -p /etc/systemd/logind.conf.d
cat > /etc/systemd/logind.conf.d/ck-nosleep.conf <<'EOF'
[Login]
IdleAction=ignore
HandleLidSwitch=ignore
HandleLidSwitchExternalPower=ignore
HandleLidSwitchDocked=ignore
EOF
systemctl kill -s HUP systemd-logind 2>/dev/null
[ "$(systemctl is-enabled suspend.target 2>/dev/null)" = masked ] && ok "suspend and hibernate are off" || bad "could not switch suspend off"
# gsettings talks to the user's session bus; with nobody logged in it needs a private bus
gset() { as_user gsettings set "$@" >/dev/null 2>&1 || as_user dbus-run-session -- gsettings set "$@" >/dev/null 2>&1; }
gget() { as_user gsettings get "$@" 2>/dev/null || as_user dbus-run-session -- gsettings get "$@" 2>/dev/null; }
if command -v gsettings >/dev/null 2>&1; then
  for s in "org.gnome.desktop.session idle-delay 0" \
           "org.gnome.desktop.screensaver lock-enabled false" \
           "org.gnome.desktop.screensaver idle-activation-enabled false" \
           "org.gnome.desktop.screensaver ubuntu-lock-on-suspend false" \
           "org.gnome.desktop.lockdown disable-lock-screen true" \
           "org.gnome.settings-daemon.plugins.power sleep-inactive-ac-type 'nothing'" \
           "org.gnome.settings-daemon.plugins.power sleep-inactive-battery-type 'nothing'" \
           "org.gnome.settings-daemon.plugins.power sleep-inactive-ac-timeout 0" \
           "org.gnome.settings-daemon.plugins.power idle-dim false"; do
    read -r schema key val <<< "$s"; gset "$schema" "$key" "$val"
  done
  case "$(gget org.gnome.desktop.session idle-delay)" in
    *" 0") ok "screen never blanks, never locks" ;;
    *) bad "screen timeout not changed (not GNOME, or the desktop settings are locked)" ;;
  esac
else
  ok "no GNOME settings here (logind sleep settings are applied)"
fi
if command -v nmcli >/dev/null 2>&1 && nmcli -t -f TYPE device 2>/dev/null | grep -q wifi && [ ! -f /etc/NetworkManager/conf.d/ck-wifi-powersave.conf ]; then
  mkdir -p /etc/NetworkManager/conf.d
  printf '[connection]\nwifi.powersave = 2\n' > /etc/NetworkManager/conf.d/ck-wifi-powersave.conf
  systemctl reload NetworkManager 2>/dev/null; fixed "Wi-Fi power saving off"
fi

step "tools"
declare -A PKG=([git]=git [curl]=curl [cc]=build-essential [pkg-config]=pkg-config [python3]=python3 [gsettings]=libglib2.0-bin [sshd]=openssh-server [certs]=ca-certificates)
have() { case $1 in sshd) [ -x /usr/sbin/sshd ] ;; certs) [ -s /etc/ssl/certs/ca-certificates.crt ] ;; *) command -v "$1" >/dev/null 2>&1 ;; esac; }
missing() { local k out=""; for k in "${!PKG[@]}"; do have "$k" || out="$out ${PKG[$k]}"; done; echo "$out"; }
APT="apt-get -o DPkg::Lock::Timeout=600 -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold -y -qq"
need=$(missing)
if [ -n "$need" ]; then
  if [ $ONLINE = 1 ]; then
    echo "   installing:$need"
    $APT update || echo "   (apt update reported errors, using the cached lists)"
    # shellcheck disable=SC2086
    $APT install $need || { dpkg --configure -a; $APT -f install; for p in $need; do $APT install --fix-missing "$p"; done; }
    need=$(missing)
    [ -z "$need" ] && fixed "installed what was missing" || bad "still missing:$need"
  else
    bad "offline, missing:$need"
  fi
else
  ok "git curl compiler python3 ssh server certificates"
fi

step "ssh and firewall"
if have sshd; then
  ssh-keygen -A >/dev/null 2>&1  # host keys, only the missing ones
  systemctl enable --now ssh >/dev/null 2>&1 || systemctl enable --now sshd >/dev/null 2>&1
  listening() { ss -ltnH 'sport = :22' 2>/dev/null | grep -q .; }
  if listening; then ok "ssh server listening on port 22"
  else
    sshd -t 2>&1 | head -3; systemctl restart ssh 2>/dev/null; sleep 2
    listening && fixed "ssh server restarted" || bad "ssh server is not listening"
  fi
fi
if [ -n "${CK_ADMIN_KEY:-}" ]; then
  KEYBODY=$(echo "$CK_ADMIN_KEY" | awk '{print $2}')
  if grep -qF "$KEYBODY" "$UH/.ssh/authorized_keys" 2>/dev/null; then ok "admin ssh key allowed"
  else
    as_user mkdir -p "$UH/.ssh"; chmod 700 "$UH/.ssh"
    echo "$CK_ADMIN_KEY" >> "$UH/.ssh/authorized_keys"; chown "$U": "$UH/.ssh/authorized_keys"; chmod 600 "$UH/.ssh/authorized_keys"
    fixed "admin ssh key added"
  fi
fi
if ufw status 2>/dev/null | grep -q "Status: active"; then
  ufw allow 22/tcp >/dev/null; ufw allow 7700:7703/tcp >/dev/null; ufw allow 7701/udp >/dev/null
  ok "ufw is on; ssh and cluster ports are open"
else
  ok "no firewall in the way (ufw off)"
fi

step "cluster-kit in $DIR"
as_user mkdir -p "$UH/Desktop" "$DIR"
if find "$DIR" ! -user "$U" -print -quit 2>/dev/null | grep -q .; then chown -R "$U": "$DIR"; fixed "files were owned by another user (a root run); fixed"; fi
git_sync() {  # works on a fresh folder, a folder copied without .git, a clone, and (after the retry below) a damaged clone
  [ -d "$DIR/.git" ] || as_user git -C "$DIR" init -q || return 1
  as_user git -C "$DIR" remote set-url origin "$REPO" 2>/dev/null || as_user git -C "$DIR" remote add origin "$REPO"
  as_user git -C "$DIR" fetch -q --depth 1 origin "$BRANCH" && as_user git -C "$DIR" checkout -q -f -B "$BRANCH" FETCH_HEAD
}
if [ $ONLINE = 1 ]; then
  if retry 3 git_sync; then
    ok "on the latest $BRANCH"
  else
    as_user rm -rf "$DIR/.git"
    retry 2 git_sync && fixed "the git data was damaged; rebuilt it" || bad "git update failed"
  fi
fi
have_cargo() { as_user cargo --version >/dev/null 2>&1; }
if ! have_cargo && [ $ONLINE = 1 ]; then
  as_user rustup default stable >/dev/null 2>&1 || retry 3 as_user bash -c 'curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal' >/dev/null 2>&1
  have_cargo && fixed "installed Rust" || bad "Rust install failed"
fi
COMMIT=$(as_user git -C "$DIR" rev-parse --short HEAD 2>/dev/null)
STAMP=$DIR/rust/target/release/.ck-built
if [ -x "$BIN" ] && [ -n "$COMMIT" ] && [ "$(cat "$STAMP" 2>/dev/null)" = "$COMMIT" ]; then
  ok "ck is built at $COMMIT"
elif ! have_cargo; then
  [ -x "$BIN" ] && ok "using the ck that is already here (no Rust to rebuild it)" || bad "no ck binary and no Rust to build one"
else
  free_kb() { df -Pk "$UH" | awk 'NR==2{print $4}'; }
  if [ "$(free_kb)" -lt 3000000 ]; then  # the build needs about 3 GB
    apt-get clean; journalctl --vacuum-size=100M >/dev/null 2>&1; rm -rf "$UH"/Desktop/cluster-kit.old-*
    [ "$(free_kb)" -ge 3000000 ] && fixed "freed disk space for the build" || bad "only $(( $(free_kb) / 1024 )) MB free; the build needs about 3000"
  fi
  J=$(nproc); MEM=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)
  [ "$MEM" -lt 4096 ] && [ "$J" -gt 2 ] && J=2   # a big parallel build runs a small PC out of memory
  [ "$MEM" -lt 1800 ] && J=1
  cb() { as_user env CARGO_BUILD_JOBS="$J" cargo build --release --manifest-path "$DIR/rust/Cargo.toml" "$@" >> /var/log/ck-build.log 2>&1; }
  echo "   building ck at $COMMIT with $J job(s): 1 to 5 minutes (the ESP stick can be unplugged now)"
  SECONDS=0
  if cb || { echo "   again after clearing incremental data"; rm -rf "$DIR/rust/target/release/incremental"; cb; } || { echo "   again without GPU support"; cb --no-default-features; }; then
    echo "$COMMIT" | as_user tee "$STAMP" > /dev/null
    fixed "built ck $COMMIT in ${SECONDS}s"
  else
    bad "ck build failed; last lines of /var/log/ck-build.log:"; tail -8 /var/log/ck-build.log | sed 's/^/        /'
  fi
fi
for l in "export CK_PRESENCE=off" "export CK_AUTH=${CK_AUTH:-off}" "export CK_FLEET=$FLEET" "export PATH=\$PATH:$DIR/rust/target/release"; do
  grep -qxF "$l" "$UH/.bashrc" 2>/dev/null || echo "$l" | as_user tee -a "$UH/.bashrc" > /dev/null
done

step "worker service (starts at boot, restarts if it stops)"
if [ -x "$BIN" ]; then
  CHANGED=0
  # Open mode: no join code, no login, no token. Standby heads the worker starts inherit it, so a promoted head is open too.
  unit() {  # name, description, ExecStart arguments -> /etc/systemd/system/<name>.service; sets CHANGED when it differs
    {
      echo "[Unit]"; echo "Description=$2"; echo "After=network-online.target"; echo "Wants=network-online.target"
      echo "[Service]"; echo "User=$U"; echo "WorkingDirectory=$DIR"
      echo "Environment=CK_FLEET=$FLEET CK_AUTH=${CK_AUTH:-off} CK_PRESENCE=off"
      [ -n "${CK_JOIN_CODE:-}" ] && echo "Environment=CK_JOIN_CODE=$CK_JOIN_CODE"
      echo "ExecStart=$BIN $3"
      echo "Restart=always"; echo "RestartSec=5"; echo "LimitNOFILE=65536"
      echo "[Install]"; echo "WantedBy=multi-user.target"
    } > "/tmp/$1.unit"
    cmp -s "/tmp/$1.unit" "/etc/systemd/system/$1.service" || { cp "/tmp/$1.unit" "/etc/systemd/system/$1.service"; systemctl daemon-reload; CHANGED=1; }
    rm -f "/tmp/$1.unit"
  }
  unit ck-worker "Cluster Kit worker" "worker${HEAD_IP:+ --head $HEAD_IP}"
  if [ "${CK_ROLE:-worker}" = head ]; then  # this PC is the head too (CK_ROLE=head): console and API on port 7702
    unit ck-head "Cluster Kit head" "head --no-local-worker"
    systemctl enable ck-head >/dev/null 2>&1
    systemctl is-active --quiet ck-head && [ $CHANGED = 0 ] || { systemctl restart ck-head; fixed "head service (re)started"; }
  fi
  BUILT=$(cat "$STAMP" 2>/dev/null)  # the commit the binary was built from; a different one than the running worker uses means restart
  [ "$(cat "$DIR/rust/target/release/.ck-started" 2>/dev/null)" != "$BUILT" ] && CHANGED=1
  systemctl enable ck-worker >/dev/null 2>&1
  PRIMARY=$(pgrep -ax ck | grep -E ' head( |$)' | grep -v -- '--standby')
  if systemctl is-active --quiet ck-worker; then
    if [ $CHANGED = 1 ]; then systemctl restart ck-worker; fixed "worker restarted on the new build"; else ok "worker service is running"; fi
  elif [ -n "$PRIMARY" ]; then
    bad "a head runs on this PC outside the service, so it was left alone (stop it, then: systemctl start ck-worker)"
  else
    pkill -x ck 2>/dev/null && sleep 2  # copies started by hand would hold the ports
    systemctl start ck-worker && fixed "worker service started" || bad "ck-worker would not start: $(systemctl status ck-worker -n 3 --no-pager 2>&1 | tail -3)"
  fi
  echo "$BUILT" | as_user tee "$DIR/rust/target/release/.ck-started" > /dev/null
  LASTLINE=""
  for _ in $(seq 1 30); do
    LASTLINE=$(journalctl -u ck-worker -n 300 -o cat 2>/dev/null | grep -E 'connected to head|lost head|cannot reach head|looking for head' | tail -1)
    case $LASTLINE in *"connected to head"*) break ;; esac
    sleep 1
  done
  case $LASTLINE in
    *"connected to head"*) ok "$LASTLINE" ;;
    *) bad "not connected to a head (${LASTLINE:-no message yet}). Start the head; if broadcast is blocked run with CK_HEAD_IP=<head ip>" ;;
  esac
else
  bad "no ck binary, so no worker service"
fi

step "result"
echo "   $(hostname)   $(hostname -I | awk '{print $1}')   commit ${COMMIT:-none}   worker: $(systemctl is-active ck-worker 2>/dev/null)"
if [ $FAILED = 0 ]; then printf '   %sDONE: all good, move to the next PC%s\n' "$G" "$N"; else printf '   %s%s PROBLEM(S): scroll up, details in %s%s\n' "$R" "$FAILED" "$LOG" "$N"; fi
printf '\a'
exit $((FAILED > 255 ? 255 : FAILED))
