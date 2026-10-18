#!/bin/sh
# Installs the Leo satellite in a Linux space, so agents can drive it.
# Run as root from the "Leo guest tools" CD: mount it, then
#   sh /mnt/install-leo-satellite.sh
# Pairs with the hub named on the CD (approve the code in Leo → Settings →
# Devices, once) and starts the satellite under systemd as a service.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
hub=${LEO_HUB_HOST:-$(cat "$here/leo-hub.txt" 2>/dev/null || true)}
name=${LEO_SATELLITE_NAME:-$(cat "$here/leo-name.txt" 2>/dev/null || hostname)}
[ -n "$hub" ] || { echo "no hub named: set LEO_HUB_HOST or run from the CD" >&2; exit 1; }
[ -f "$here/leo-satellited" ] || { echo "no Linux satellite on this CD" >&2; exit 1; }
id leo >/dev/null 2>&1 || useradd --system --create-home --home-dir /var/lib/leo --shell /bin/bash leo
install -m 0755 "$here/leo-satellited" /usr/local/bin/leo-satellited
install -d -o leo -g leo -m 0750 /var/lib/leo
if [ -f /var/lib/leo/satellite.env ]; then
  :
elif [ -f "$here/satellite.env" ]; then
  # Leo paired this space when it made it: the token is on the CD.
  install -m 0600 -o leo -g leo "$here/satellite.env" /var/lib/leo/satellite.env
else
  echo "Pairing with $hub as $name — approve the code in Leo → Settings → Devices."
  LEO_DATA_DIR=/var/lib/leo /usr/local/bin/leo-satellited pair "$hub" --name "$name"
  chown leo:leo /var/lib/leo/satellite.env
fi
cat > /etc/systemd/system/leo-satellited.service <<UNIT
[Unit]
Description=Leo satellite
After=network-online.target
Wants=network-online.target
[Service]
User=leo
Environment=LEO_DATA_DIR=/var/lib/leo
ExecStart=/usr/local/bin/leo-satellited run
Restart=always
RestartSec=5
[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now leo-satellited
echo "Done. Claim $name on the hub (Settings → Devices) so people's commands are accepted."
