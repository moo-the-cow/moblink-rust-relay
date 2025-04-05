#!/usr/bin/env bash

set -e

WORKSPACE=moblink-rust-install
REPO="datagutt/moblink-rust"
LATEST_RELEASE_URL="https://github.com/$REPO/releases/latest/download"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"

# Detect architecture
ARCH=$(uname -m)
case $ARCH in
x86_64)
	TARGET="x86_64-unknown-linux-gnu"
	;;
aarch64)
	TARGET="aarch64-unknown-linux-gnu"
	;;
*)
	echo "Unsupported architecture: $ARCH"
	exit 1
	;;
esac

rm -rf $WORKSPACE
mkdir $WORKSPACE
cd $WORKSPACE

systemctl stop moblink-streamer || true
systemctl stop moblink-relay-service || true

# Download architecture-specific binaries
wget "$LATEST_RELEASE_URL/moblink-relay-$TARGET"
wget "$LATEST_RELEASE_URL/moblink-relay-service-$TARGET"
wget "$LATEST_RELEASE_URL/moblink-streamer-$TARGET"

# Make binaries executable and move to /usr/local/bin
chmod +x moblink-relay-$TARGET moblink-relay-service-$TARGET moblink-streamer-$TARGET
mv moblink-relay-$TARGET /usr/local/bin/moblink-relay
mv moblink-relay-service-$TARGET /usr/local/bin/moblink-relay-service
mv moblink-streamer-$TARGET /usr/local/bin/moblink-streamer

# Use local systemd files
cp "$SCRIPT_DIR/systemd/"* /etc/systemd/system/

systemctl enable moblink-streamer
systemctl start moblink-streamer

systemctl enable moblink-relay-service
systemctl start moblink-relay-service

rm -rf $WORKSPACE
