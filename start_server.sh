#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

echo "Starting astro inventory server on http://127.0.0.1:5000 ..."
exec .venv/bin/python astro_inventory_server.py 2>&1 | tee -a server.log
