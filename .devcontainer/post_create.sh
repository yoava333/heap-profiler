set -euxo pipefail

curl -LsSf https://astral.sh/uv/install.sh | sh
uv python install 3.11

sudo apt-get update
sudo apt-get install -y python3-dev python3-pip python3-venv libclang-dev

uv tool install maturin
uv tool install cargo-deny
uv tool install uniffi-bindgen