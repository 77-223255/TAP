# TAP

把电脑里正在播放的声音，像麦克风一样接进 DAW 的轨道。**PipeWire / CLAP / Linux**。

![TAP](docs/preview.png)

## 用

1. 轨道插 **TAP** → 面板空白处右键 → `Capture` 打勾（默认关）
2. 轨道录音模式 `Record: output (stereo)`
3. 按录音键

**只录不响**（像 Live 那样）：轨道 `Route` → 取消 `Master send`，录音照样能录到。

旋钮 ±24 dB（双击回 0）；右键菜单里还有 `Zoom 50–300%`。

详细的 REAPER 设置、导出和排查 → **[docs/reaper.md](docs/reaper.md)**

## 装

```bash
cargo build --release && ./install.sh   # → ~/.clap/tap.clap
```

REAPER 里 `Options → Preferences → Plug-ins → CLAP → Re-scan`。

依赖：`rust`、`pipewire`、`libx11`、任意中文字体。

## 原理

抓 sink monitor 一定会啸叫（monitor 里含宿主自己的输出）。所以换个做法：

> 把每个应用的播放流节点直接连进插件输入口，并排除宿主进程自己 —— 结构上不可能成环。

输出 = 输入 + 捕获（`Capture` 关掉时是纯透传）。

## 开发

```bash
cargo run --release --example probe       # 验证抓音
cargo run --release --example selftest    # 验证不会抓到自己（不会啸叫）
cargo run --release --example preview -- docs/preview.png 2 menu
```

GPL-3.0-or-later
