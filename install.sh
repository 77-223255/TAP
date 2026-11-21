#!/usr/bin/env bash
# 编译并安装 TAP 到 ~/.clap（不需要 sudo）。
# 只装 CLAP —— 没有 VST2 / VST3 / LV2 版本，也不打算维护。
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release
mkdir -p "$HOME/.clap"
install -m 755 target/release/libtap.so "$HOME/.clap/tap.clap"

# 旧名字（NeiLu）的残留顺手清掉
rm -f "$HOME/.clap/neilu.clap"

echo "装好了：$HOME/.clap/tap.clap"
echo "REAPER 里：Options → Preferences → Plug-ins → CLAP → Re-scan，然后 FX 里搜 TAP"
