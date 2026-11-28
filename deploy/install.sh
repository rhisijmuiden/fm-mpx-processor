#!/bin/sh
# PILOT installer for Debian 13 (native, systemd, no containers).
#
#   ./deploy/install.sh            build from this checkout and install
#   ./deploy/install.sh --channel production
#
# Safe to run again: it updates the binary and the unit, keeps the database.
set -eu

CHANNEL=test
while [ $# -gt 0 ]; do
  case "$1" in
    --channel) CHANNEL="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

ROOT=$(cd "$(dirname "$0")/.." && pwd)
export DEBIAN_FRONTEND=noninteractive

need_pkgs=""
for p in ffmpeg alsa-utils libasound2-dev pkg-config build-essential cargo rustc ca-certificates; do
  dpkg -s "$p" >/dev/null 2>&1 || need_pkgs="$need_pkgs $p"
done
if [ -n "$need_pkgs" ]; then
  apt-get update -q
  # shellcheck disable=SC2086
  apt-get install -y -q $need_pkgs
fi

# ALSA loopback for the MicroMPX encoder (needs the generic kernel, not -cloud).
echo snd-aloop > /etc/modules-load.d/pilot.conf
echo 'options snd-aloop index=0 id=Loopback pcm_substreams=2' > /etc/modprobe.d/pilot-aloop.conf
modprobe snd-aloop 2>/dev/null || echo "warning: snd-aloop not available in this kernel (install linux-image-amd64)"

echo "building PILOT engine..."
cd "$ROOT/engine"
PILOT_GIT_SHA=$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown) \
  cargo build --release --locked
install -m 0755 target/release/pilot /usr/local/bin/pilot.new
mv /usr/local/bin/pilot.new /usr/local/bin/pilot

id pilot >/dev/null 2>&1 || useradd --system --home /var/lib/pilot --shell /usr/sbin/nologin --groups audio pilot
install -d -o pilot -g pilot -m 0750 /var/lib/pilot
install -d -m 0755 /etc/pilot
[ -f /etc/pilot/pilot.env ] || cat > /etc/pilot/pilot.env <<ENV
# PILOT environment. PILOT_API_TOKEN protects all write calls when set.
PILOT_CHANNEL=$CHANNEL
#PILOT_API_TOKEN=
ENV
sed -i "s/^PILOT_CHANNEL=.*/PILOT_CHANNEL=$CHANNEL/" /etc/pilot/pilot.env

install -m 0644 "$ROOT/deploy/pilot.service" /etc/systemd/system/pilot.service
systemctl daemon-reload
systemctl enable pilot.service >/dev/null
systemctl restart pilot.service
sleep 2
systemctl --no-pager --lines=5 status pilot.service || true
echo "PILOT $(/usr/local/bin/pilot --version) installed, channel $CHANNEL, management on port 8080"
