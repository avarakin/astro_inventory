#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

echo "Starting astro inventory server on http://127.0.0.1:5000 ..."
exec ./target/release/astro_inventory --root /data/Astro/CCD 2>&1 | tee -a server.log
