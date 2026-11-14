#!/usr/bin/env bash
# Provision one Ubuntu lab PC for Cluster Kit. Idempotent and self-healing: every item is checked first, repaired when
# it is wrong, and checked again; a PC that is already fine finishes in a few seconds. The ESP32 stick in hid/ types this.
#   sudo bash provision.sh
# Settings (environment): CK_HEAD_IP    pin the head (default: workers find it by broadcast)
#                         CK_FLEET      open | secured (default open)      CK_JOIN_CODE  for a secured fleet
#                         CK_BRANCH     git branch (default main)           CK_ADMIN_KEY  ssh public key line to allow in
#                         CK_ROLE=head  also run the head here as a service  CK_AUTH  off (default): no login, no token

# These helpers can be checked without executing machine provisioning.
ck_apt() { apt-get -o DPkg::Lock::Timeout=600 -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold -y "$@"; }
ck_install_packages() {
  if [ "${CK_FORCE_APT_UNLOCK:-0}" = 1 ]; then ck_unlock_apt || return 1; fi
  ck_apt update || echo "apt index update failed; trying cached package lists" >&2
  ck_apt install "$@" && return 0
  dpkg --configure -a || echo "dpkg still needs dependency repair" >&2
  ck_apt --fix-broken install || return 1
  dpkg --configure -a || return 1
  ck_apt update || echo "apt index update failed after repair" >&2
  ck_apt --fix-missing install "$@"
}

# Force-delete package lock files only after their users have exited.
ck_unlock_apt() {
  command -v fuser >/dev/null 2>&1 || { echo "fuser unavailable; leaving apt locks in place" >&2; return 0; }
  local waited=0 limit=${CK_APT_LOCK_WAIT:-600}
  local locks=(/var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/lib/apt/lists/lock /var/cache/apt/archives/lock)
  while fuser "${locks[@]}" >/dev/null 2>&1; do
    [ "$waited" -lt "$limit" ] || { echo "package manager is still running; refusing to delete its locks" >&2; return 1; }
    sleep 2; waited=$((waited + 2))
  done
  rm -f "${locks[@]}"
}

ck_download() {
  curl --retry 2 --connect-timeout 15 -fsSL "$1" -o "$2" ||
    wget -qO "$2" "$1" ||
    python3 -c 'import sys,urllib.request; urllib.request.urlretrieve(sys.argv[1],sys.argv[2])' "$1" "$2"
}

# Validate and smoke-test a staged release before stopping or replacing a worker.
ck_install_release() (
  local repo=$1 bin=$2 stamp=$3 stage tag metadata commit digest target output
  [ "$(uname -m)" = x86_64 ] || { echo "No Linux release for this architecture" >&2; return 1; }
  stage=$(mktemp -d) || return 1
  trap 'rm -rf "$stage"' EXIT
  ck_download "https://api.github.com/repos/$repo/releases?per_page=20" "$stage/releases.json" || return 1
  tag=$(python3 - "$stage/releases.json" <<'JSON'
import json,re,sys
for release in json.load(open(sys.argv[1])):
    tag = release.get('tag_name', '')
    if not release.get('draft') and not release.get('prerelease') and re.fullmatch(r'linux-[0-9a-f]{40}', tag):
        print(tag)
        break
else:
    sys.exit('No tested Linux release is available')
JSON
  ) || return 1
  local base="https://github.com/$repo/releases/download/$tag"
  ck_download "$base/linux-manifest.json" "$stage/manifest.json" || return 1
  metadata=$(python3 - "$stage/manifest.json" "$tag" <<'JSON'
import json,re,sys
m = json.load(open(sys.argv[1]))
if m.get('commit') != sys.argv[2][6:] or m.get('target') != 'x86_64-unknown-linux-musl' or not re.fullmatch(r'[0-9a-f]{64}', m.get('sha256', '')):
    sys.exit('Invalid Linux release manifest')
print(m['commit'], m['sha256'], m['target'])
JSON
  ) || return 1
  read -r commit digest target <<< "$metadata"
  ck_download "$base/ck-linux-x86_64" "$stage/ck" || return 1
  echo "$digest  $stage/ck" | sha256sum -c - >/dev/null || return 1
  chmod 755 "$stage/ck" || return 1
  output=$("$stage/ck" 2>&1) || true
  case $output in *"usage: ck "*) ;; *) echo "Linux release will not run on this PC; using a source build" >&2; return 1 ;; esac
  mkdir -p "$(dirname "$bin")" || return 1
  cp "$stage/ck" "$bin.new" || return 1
  if [ "${CK_REPLACE:-0}" = 1 ]; then
    ck_stop_existing || { rm -f "$bin.new"; return 1; }
  fi
  mv -f "$bin.new" "$bin" || return 1
  printf '%s\n' "release:$commit:$target" > "$stamp"
)

ck_git_sync() {
  local dir=$1 repo=$2 branch=$3 stage backup item
  if [ ! -d "$dir/.git" ]; then
    mkdir -p "$(dirname "$dir")" || return 1
    stage=$(mktemp -d "${dir}.clone.XXXXXX") || return 1
    git clone --single-branch --branch "$branch" "$repo" "$stage/repo" || return 1
    if [ -d "$dir" ]; then
      backup=$(mktemp -d "${dir}.before-git.XXXXXX") || return 1
      mv "$dir" "$backup/cluster-kit" || return 1
      echo "Previous folder saved in $backup/cluster-kit"
    fi
    mv "$stage/repo" "$dir" || return 1
    rmdir "$stage"
    if [ -n "${backup:-}" ]; then
      for item in ck-data rust/target; do
        if [ -d "$backup/cluster-kit/$item" ]; then
          mkdir -p "$(dirname "$dir/$item")"
          mv "$backup/cluster-kit/$item" "$dir/$item" || return 1
          if [ "$item" = rust/target ]; then rm -f "$dir/rust/target/release/.ck-built"; fi
        fi
      done
    fi
    return 0
  fi
  [ -z "$(git -C "$dir" status --porcelain --untracked-files=no)" ] || {
    echo "Local tracked edits in $dir; refusing to overwrite them" >&2; return 1;
  }
  # Single-branch clones need a fetch mapping before Git can track a newly deployed branch.
  if ! git -C "$dir" config --get-all remote.origin.fetch | grep -Fx -e '+refs/heads/*:refs/remotes/origin/*' -e "+refs/heads/$branch:refs/remotes/origin/$branch" >/dev/null; then
    git -C "$dir" config --add remote.origin.fetch "+refs/heads/$branch:refs/remotes/origin/$branch" || return 1
  fi
  git -C "$dir" fetch origin "refs/heads/$branch:refs/remotes/origin/$branch" || return 1
  if [ "$(git -C "$dir" branch --show-current)" != "$branch" ]; then
    git -C "$dir" merge-base --is-ancestor HEAD FETCH_HEAD || {
      echo "Current branch has commits outside $branch; refusing to discard them" >&2; return 1;
    }
    if git -C "$dir" show-ref --verify --quiet "refs/heads/$branch"; then
      git -C "$dir" merge-base --is-ancestor "$branch" FETCH_HEAD || return 1
      git -C "$dir" checkout "$branch" || return 1
    else
      git -C "$dir" checkout -b "$branch" --track "origin/$branch" || return 1
    fi
  fi
  git -C "$dir" merge --ff-only "origin/$branch"

}

ck_stop_existing() {
  # The visible launcher explicitly requests replacing all local ck instances.
  systemctl stop ck-worker ck-head ck-agent 2>/dev/null || true
  pkill -TERM -x ck 2>/dev/null || [ "$?" -eq 1 ] || return 1
  local n
  for n in {1..10}; do pgrep -x ck >/dev/null || return 0; sleep 1; done
  pkill -KILL -x ck 2>/dev/null || [ "$?" -eq 1 ] || return 1
  sleep 1
  ! pgrep -x ck >/dev/null
}

ck_banner() {
  local user=$1 ip ips
  ips=$(hostname -I 2>/dev/null)
  printf '\n\033[1;36m============================================================\n'
  printf '  CLUSTER KIT WORKER  |  %s\n' "$(hostname)"
  printf '============================================================\n'
  if command -v figlet >/dev/null 2>&1; then figlet -f small "$(hostname)"; fi
  if [ -n "$ips" ]; then
    for ip in $ips; do
      printf '\n  IP: %s\n' "$ip"
      if command -v figlet >/dev/null 2>&1; then figlet -f small "$ip"; fi
      printf '  SSH: ssh %s@%s\n' "$user" "$ip"
    done
  else
    printf '  IP: unavailable — check the network connection\n'
  fi
  if ss -ltnH 'sport = :22' 2>/dev/null | grep -q .; then
    printf '\n  SSH: listening on port 22\n'
  else
    printf '\n  SSH: NOT listening — see the repair errors above\n'
  fi
  printf '  Fleet: %s | Authentication: %s | Head: %s\n' "${CK_FLEET:-open}" "${CK_AUTH:-off}" "${CK_HEAD_IP:-LAN discovery}"
  printf '============================================================\033[0m\n\n'
}

# Provisioning starts here

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
CK_FORCE_APT_UNLOCK=${CK_FORCE_APT_UNLOCK:-1}
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
ck_banner "$U"
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

step "tools"
declare -A PKG=([git]=git [curl]=curl [python3]=python3 [gsettings]=libglib2.0-bin [sshd]=openssh-server [certs]=ca-certificates [figlet]=figlet [ss]=iproute2 [pgrep]=procps)
have() { case $1 in sshd) [ -x /usr/sbin/sshd ] ;; certs) [ -s /etc/ssl/certs/ca-certificates.crt ] ;; *) command -v "$1" >/dev/null 2>&1 ;; esac; }
missing() { local k out=""; for k in "${!PKG[@]}"; do have "$k" || out="$out ${PKG[$k]}"; done; echo "$out"; }
APT="apt-get -o DPkg::Lock::Timeout=600 -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold -y -qq"
need=$(missing)
if [ -n "$need" ]; then
  if [ $ONLINE = 1 ]; then
    echo "   installing:$need"
    # shellcheck disable=SC2086
    ck_install_packages $need || bad "package repair failed; see apt/dpkg output above"
    need=$(missing)
    [ -z "$need" ] && fixed "installed what was missing" || bad "still missing:$need"
  else
    bad "offline, missing:$need"
  fi
else
  ok "git curl python3 ssh server certificates"
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
git_sync() {
  as_user bash -c "$(declare -f ck_git_sync); ck_git_sync \"\$@\"" _ "$DIR" "$REPO" "$BRANCH"
}
if [ $ONLINE = 1 ]; then
  if git_sync; then ok "on the latest $BRANCH"
  else bad "git update failed; existing files were preserved"; exit 1; fi
else
  bad "cannot update the worker while offline"; exit 1
fi
COMMIT=$(as_user git -C "$DIR" rev-parse --short HEAD 2>/dev/null)
STAMP=$DIR/rust/target/release/.ck-built
if [ "${CK_BUILD_SOURCE:-0}" != 1 ] && [ "$BRANCH" = main ] && ck_install_release "${CK_RELEASE_REPO:-TheDivyanshShukla/cluster-kit}" "$BIN" "$STAMP"; then
  chown "$U": "$BIN" "$STAMP"
  if [ "${CK_REPLACE:-0}" = 1 ]; then systemctl disable ck-head ck-agent >/dev/null 2>&1 || true; fi
  fixed "installed verified Linux CPU release (no Rust or compiler needed)"
else
  step "source build (release unavailable, incompatible, or explicitly disabled)"
  if ! have cc || ! have pkg-config; then
    ck_install_packages build-essential pkg-config || { bad "compiler packages unavailable"; exit 1; }
  fi
  if [ "${CK_REPLACE:-0}" = 1 ]; then
    step "replacing existing ck instances"
    ck_stop_existing || { bad "old ck process did not stop"; exit 1; }
    systemctl disable ck-head ck-agent >/dev/null 2>&1 || true
  fi

  have_cargo() { as_user cargo --version >/dev/null 2>&1; }
  if ! have_cargo && [ $ONLINE = 1 ]; then
    as_user rustup default stable >/dev/null 2>&1 || retry 3 as_user bash -c 'curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal' >/dev/null 2>&1
    have_cargo && fixed "installed Rust" || bad "Rust install failed"
  fi
  COMMIT=$(as_user git -C "$DIR" rev-parse --short HEAD 2>/dev/null)
  BUILD_ID="$COMMIT:$(uname -s):$(uname -m)"
  if [ -x "$BIN" ] && [ -n "$COMMIT" ] && [ "$(cat "$STAMP" 2>/dev/null)" = "$BUILD_ID" ]; then
    ok "ck is built at $COMMIT"
  elif ! have_cargo; then
    bad "Rust is unavailable; refusing to start an out-of-date worker"; exit 1
  else
    free_kb() { df -Pk "$UH" | awk 'NR==2{print $4}'; }
    if [ "$(free_kb)" -lt 3000000 ]; then  # the build needs about 3 GB
      apt-get clean; journalctl --vacuum-size=100M >/dev/null 2>&1; rm -rf "$UH"/Desktop/cluster-kit.old-*
      [ "$(free_kb)" -ge 3000000 ] && fixed "freed disk space for the build" || bad "only $(( $(free_kb) / 1024 )) MB free; the build needs about 3000"
    fi
    J=$(nproc); MEM=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)
    [ "$MEM" -lt 4096 ] && [ "$J" -gt 2 ] && J=2   # a big parallel build runs a small PC out of memory
    [ "$MEM" -lt 1800 ] && J=1
    cb() { as_user env CARGO_BUILD_JOBS="$J" cargo build --release --locked --manifest-path "$DIR/rust/Cargo.toml" "$@" >> /var/log/ck-build.log 2>&1; }
    echo "   building ck at $COMMIT with $J job(s): 1 to 5 minutes (the ESP stick can be unplugged now)"
    SECONDS=0
    if cb || { echo "   again after clearing incremental data"; rm -rf "$DIR/rust/target/release/incremental"; cb; } || { echo "   again without GPU support"; cb --no-default-features; }; then
      echo "$BUILD_ID" | as_user tee "$STAMP" > /dev/null
      fixed "built ck $COMMIT in ${SECONDS}s"
    else
      bad "ck build failed; last lines of /var/log/ck-build.log:"; tail -8 /var/log/ck-build.log | sed 's/^/        /'; exit 1
    fi
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
  SINCE=$(systemctl show ck-worker -p ActiveEnterTimestamp --value)
  LASTLINE=""
  for _ in $(seq 1 30); do
    LASTLINE=$(journalctl -u ck-worker --since "${SINCE:-today}" -n 300 -o cat 2>/dev/null | grep -E 'connected to head|lost head|cannot reach head|looking for head' | tail -1)
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
ck_banner "$U"
if [ "${CK_FOLLOW:-0}" = 1 ] && systemctl is-active --quiet ck-worker; then
  echo "Live worker log below: 'connected to head' confirms a connection. Ctrl+C closes this view; the worker keeps running."
  journalctl -u ck-worker --since "${SINCE:-today}" -f -o cat
fi
printf '\a'
exit $((FAILED > 255 ? 255 : FAILED))
