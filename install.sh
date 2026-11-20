#!/usr/bin/env bash
# 编译并把插件装到 ~/.clap（不需要 sudo）
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release
mkdir -p "$HOME/.clap"
install -m 755 target/release/libneilu.so "$HOME/.clap/neilu.clap"

echo "装好了：$HOME/.clap/neilu.clap"
echo "在 Reaper 里：Options → Preferences → Plug-ins → CLAP → Re-scan"
