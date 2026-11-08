#!/usr/bin/env bash
set -euxo pipefail

# Valgrind provides the instruction counts used by the performance gate.
sudo apt-get update
sudo apt-get install -y --no-install-recommends valgrind

curl -LsSf https://astral.sh/uv/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"

# Builds the extension into .venv along with the test dependencies.
uv sync --locked
