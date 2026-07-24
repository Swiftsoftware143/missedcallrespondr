#!/bin/bash
set -e
cd /opt/swift/missedcall_respondr
for i in $(seq 1 60); do
  if mkdir /tmp/rust-build.lock 2>/dev/null; then break; fi
  sleep 2
  if [ "$i" -eq 60 ]; then echo "ERROR: Could not acquire lock"; exit 1; fi
done
trap 'rmdir /tmp/rust-build.lock 2>/dev/null' EXIT
git pull origin main
CARGO_BUILD_JOBS=1 /root/.cargo/bin/cargo build --release
systemctl restart missedcallrespondr
sleep 1
systemctl --no-pager status missedcallrespondr --no-pager | head -10
echo "=== Deploy complete ==="
