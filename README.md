# TAP

**把电脑里正在播放的声音，像麦克风一样接进 DAW 的轨道。**
Linux / PipeWire 专用的 CLAP 插件。

![screenshot](docs/preview.png)

> 这个仓库以前叫 NeiLu，插件名太傻，改叫 **TAP**（从水管上接个龙头）。

## 只做一件事

取音。插件不选设备、不录音、不写文件、不做编辑——落盘、剪辑、混音全用宿主自带的。

**永远抓所有应用的播放流，并且排除宿主自己**，所以不需要虚拟声卡、不需要 loopback 设备，
结构上也不可能啸叫（详见下面「为什么这么设计」）。

## 为什么这么设计（重要）

最直觉的做法是「抓 sink 的 monitor」。**那样一定会啸叫**：monitor 里包含宿主自己的输出，
于是 插件输出 → 声卡 → monitor → 插件输入 形成增益为 1 的环路，几秒内就炸。

Ardour 论坛和 OBS 社区对这件事的结论一致：抓 monitor 就必须断开宿主输出、
或者另建虚拟 sink、或者靠「关监听 + 静音轨道」来躲。都不适合「插上就用」。

所以 TAP 走 OBS 的 *Application Audio Capture* 那条路：

> **不抓 monitor。把每个应用的播放流节点直接连进插件的输入口，并显式排除本进程（宿主自己）。**

于是宿主的输出永远不会被自己抓回来 —— **结构上不可能成环**。

## 它是插入式效果：输入永远原样透传

输出 = 输入 + 我们捕获的音频；不叠加捕获时输出 = 输入本身。

所以放在**同一轨道**上完全没问题：这条轨上已经录好的片段重放会照常流过插件。

## 用法（REAPER 为例）

1. 轨道上插入 **TAP**
2. 轨道录音模式：右键录音按钮 / 右键轨道 → `Record: output (stereo)`，
   **再点一次选择子模式**：`post fader` / `post-fx pre-fader` / `pre fx`
3. 按录音键。轨道上就会出现捕获到的音频

**录音时别开轨道的监听。** 源本来就在响（浏览器/播放器），监听只会再叠一份晚几十毫秒的副本。

### 让 REAPER 不放声音（像 Live 那样只录不响）

插件没法控制宿主的输出路由，这一步在 REAPER 里做一次就行：

1. 点轨道的 **路由按钮**（TCP 上的 `Route`）
2. 取消勾选 **Master send**

这样这条轨不会再往 master 送音频 —— 录音期间你只听见源本身，不会有第二份；
而 `Record: output` 的取样点在发送到 master 之前，所以照样能录到。

想听录好的片段时，再把这个勾打开（不录音时插件只是透传，不会掺入实时声音）。

## 界面

纯平（flat），没有渐变/高光/倒角，也没有一个多余的字：

- **一圈电平**：套在旋钮外面的圆环，两声道取大的那个；绿（-6 dB 以下）/ 橙（以上），
  白色小刻度是峰值保持
- **旋钮**：±24 dB，拖动调节；**双击回 0 dB**；中间直接显示 dB 数
- 外圈那颗**橙色小刻度**是当前增益位置
- 默认缩放 **150%**

全部控制都在**右键菜单**里（窗口任意位置右键）：

- `Capture`：开/关捕获（关掉后插件变成纯透传，连 PipeWire 客户端都不开）
- `Only while recording`：只在录音时叠加捕获（默认开）。关掉它就变成「一直叠加」
- `Zoom 50% / 100% / 150% / 200% / 250% / 300%`：界面缩放（默认 150%，按档位跳）

界面按 **120 fps** 重绘（放大到 200%/300% 时自动降到 60/40 fps），字形有缓存，走 MIT-SHM 上屏。

## 导出轨道音频（REAPER）

**最快**：选中轨道 → `File → Render…`

- **Source**: `Selected tracks (stems)`
- **Bounds**: `Entire project` 或 `Time selection`
- **Output**: `WAV` / `24 bit PCM`
- 填好 Directory / File name → `Render 1 file`

**就地生成一条含音频的新轨**：右键轨道 → `Render/freeze tracks` →
`Render tracks to stereo stem tracks (and mute originals)`，多出来的 `[stem]` 轨里就是渲染好的音频。

渲染是离线跑的，不录音时插件只透传，所以不会把当时的系统声音混进去。

## 安装

```bash
cargo build --release
./install.sh          # 装到 ~/.clap/tap.clap
```

然后 REAPER 里 `Options → Preferences → Plug-ins → CLAP → Re-scan`，FX 浏览器搜 **TAP**。

依赖（Arch）：`rust`、`pipewire`、`libx11`、`noto-fonts-cjk`（或任何带中文的字体）。运行时不需要额外装任何东西。

## 采样率

插件**不强制**采样率：PipeWire 那边按声卡当前的图速率协商，协商结果回读。
声卡 44.1 kHz 而工程 48 kHz 时插件自己重采样（线性插值 + 按缓冲填充量做 ±2% 的漂移补偿）。

## 工作原理

```
应用播放流 ──link-factory──▶ TAP 的输入口 ──▶ 无锁环形缓冲 ──▶ 音频线程 ──▶ 输出 = 输入 + 捕获
（宿主的流被排除）                                    ▲
                                              按填充量微调播放速率
```

- 枚举 registry 里的节点：`media.class == "Stream/Output/Audio"` 就是应用播放流；
  节点 props 里常常没有 pid，要顺着 `client.id` 去 Client 对象上找 `application.process.id`
  （兜底 `pipewire.sec.pid`），和 `std::process::id()` 比较来排除宿主
- 端口对端口用 `link-factory` 连接，单声道应用同时接左右两路；registry 事件驱动 + 250ms 兜底重扫
- `process()` 成功必须返回 `CLAP_PROCESS_CONTINUE`(1)。返回 0 是 `CLAP_PROCESS_ERROR`，
  宿主会把这一块的输出**整个人丢掉**
- 输出 buffer 的 `constant_mask` 必须自己清零，否则宿主会把逐样本的音频当成恒定值读
- 只叠加捕获的时机由 `CLAP_TRANSPORT_IS_RECORDING` 决定；宿主从没给过走带信息时不 gate

## 已知限制

- 只支持 X11 窗口（Wayland 原生窗口没做，REAPER 本身走 XWayland 所以没问题）
- 抓的是**所有**应用播放流（除宿主自己），暂时不能只挑某一个应用
- 只在 REAPER + PipeWire 1.6 / Hyprland 上验证过
- 录音门控依赖 CLAP transport 的录音标志，REAPER 要 ≥ 7.66
- 没有 Windows / macOS 支持（这两边本来就不需要它）

## 开发

```bash
# 不开 DAW，直接验证 PipeWire 这一侧抓没抓到声音
cargo run --release --example probe

# 自测：本进程自己放 1234Hz，捕获里不该出现这个频率（证明宿主输出被排除 → 不会啸叫）
cargo run --release --example selftest

# 重新渲染预览图（数字是 ZOOM 档位 0..5，最后一个参数给 menu 就画上右键菜单）
cargo run --release --example preview -- docs/preview.png 1
cargo run --release --example preview -- /tmp/menu.png 1 menu

# 调试：打印 CLAP 回调；或让插件忽略抓音、直接吐 440Hz
TAP_DEBUG=1 reaper
TAP_DEBUG_TONE=1 reaper
```

## License

GPL-3.0-or-later
