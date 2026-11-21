# TAP

**把电脑里正在播放的声音，像麦克风一样接进 DAW 的轨道。**
Linux / PipeWire 专用的 CLAP 插件。装上、点一下，轨道就开始长音频——不用虚拟声卡、不用 `parecord`、不用来回搬文件。

![screenshot](docs/preview.png)

> 这个仓库以前叫 NeiLu，插件名太傻，改叫 **TAP**（从水管上接个龙头）。

## 只做一件事

取音。插件不选设备、不录音、不写文件、不做编辑——落盘、剪辑、混音全用宿主自带的。

**永远抓系统默认输出的 monitor**（`stream.capture.sink = true`），不需要虚拟声卡，
不需要选输入源，没有 loopback 设备也照样能用。所以界面上没有设备列表、没有开关按钮：
插上就抓，宿主要开始处理时它就在跑。

## 为什么需要它

USB 声卡（比如 Steinberg UR12）在 Linux 上没有硬件 loopback，`dspMixFx` 那套是 Windows 驱动的功能。
想在 Linux 里内录，通常得命令行把 PipeWire 的 monitor 录成 wav 再拖回工程。

TAP 把这件事搬进插件：它自己作为 PipeWire 客户端去抓任意 Sink 的 monitor，
把音频吐到插件输出端口，「录进轨道」就变成普通的录音动作。

## 用法（REAPER 为例）

1. 在轨道上插入 **TAP**（插上就开始抓系统默认输出）
2. 轨道录音模式：右键录音按钮 / 右键轨道 → `Record: output (stereo)`，
   **再点一次选择子模式**：`post fader` / `post-fx pre-fader` / `pre fx`
   （想没了推子影响就选 `post-fx pre-fader`）
3. 想边录边听：打开轨道的 **录音监听**（Record monitoring）
4. 按录音键。轨道上就会出现捕获到的音频

想单独抓某个程序（浏览器、播放器）：把它指到某个输出设备，然后在列表里选那个设备。

### 轨道没反应？按这个顺序看

1. **插件界面的电平在动吗？** 在动 = 抓音没问题，问题在宿主侧；不动 = 抓音没起来（看底栏错误文本）
2. **宿主在处理这个轨道吗？** REAPER 只在需要时才跑 FX 链：arm + 监听、或播放、或录音。
   一条既没 arm、也没监听、也没在播的轨道，FX 链是不跑的
3. **录音模式的子选项**：`Record: output` 下面还有 `post fader / post-fx pre-fader / pre fx`，
   选错了就会把推子/静音带进去
4. **FX 的“静音自动 bypass”**：REAPER 可以按静音自动旁通 FX（右键插件名可关），
   这条轨道在静音时会被它绕过去
5. 还是不行就跑一次 `TAP_DEBUG_TONE=1 reaper`：插件会忽略抓音、直接输出 440 Hz 正弦。
   这时轨道里能听到 440 Hz 就说明插件的输出通路是通的，问题在抓音；听不到就是宿主侧

## 界面

- **LED**：实体感的指示灯，亮度/光晕跟着音频走（LIVE / IDLE / ERROR）
- **GAIN**：±24 dB 的旋钮，拖动调节；**双击回 0 dB**
- **OUTPUT**：两条电平 + 峰值保持 + dB 刻度
- **ZOOM**：`50 / 100 / 150 / 200 / 250 / 300 %`，尺寸按档位跳，不是自由拉伸
- 底栏显示宿主速率、声卡速率（不一致会标 `RESAMPLED`）、缓冲毫秒和丢帧数
- 界面按 **120 fps** 重绘（放大到 200%/300% 时自动降到 60/40 fps，省 CPU），
  字形有缓存，走 MIT-SHM 上屏

## 安装

```bash
cargo build --release
./install.sh          # 装到 ~/.clap/tap.clap
```

然后在 REAPER 里：`Options → Preferences → Plug-ins → CLAP → Re-scan`，FX 浏览器搜 **TAP**。

依赖（Arch）：`rust`、`pipewire`、`libx11`、`noto-fonts-cjk`（或任何带中文的字体）。运行时不需要额外装任何东西。

## 采样率

插件**不强制**采样率：PipeWire 那边按声卡当前的图速率协商，协商结果回读到界面（`CARD`）。
如果声卡是 44.1 kHz 而工程是 48 kHz，插件自己重采样（线性插值 + 按缓冲填充量做 ±2% 的漂移补偿），
不会因为两边时钟不同步而慢慢积累成爆音。

## 工作原理

```
PipeWire 线程 ──pw_stream(sink monitor)──▶ 无锁环形缓冲 ──▶ 音频线程 ──▶ 插件输出端口
                                                     ▲
                                             按填充量微调播放速率
                                            （和 DAW 时钟的漂移补偿）
```

- 抓 monitor 用 `stream.capture.sink = true`（跟随系统默认输出）
- `process()` 成功必须返回 `CLAP_PROCESS_CONTINUE`(1)。返回 0 是 `CLAP_PROCESS_ERROR`，
  宿主会把这一块的输出 buffer **整个人丢掉**——这也是「插件有电平、轨道没声音」的原因之一
- 输出 buffer 的 `constant_mask` 必须自己清零。宿主从不清理这个字段，而它代表
  「这个 block 里每个声道都是第 0 个样本的恒定值，你可以只读一个样本」。
  REAPER 的静音优化会把输出 buffer 预标成恒定/静音；插件不清它，宿主就把我们
  逐样本的音频按「采样保持」读成噪声——表现是**插件电平在动、轨道却像没声音**
- 捕获跑在 PipeWire 自己的实时线程里，只做一件事：把交错立体声写进环形缓冲，不分配内存、不加锁
- 音频线程按 60 ms 的缓冲目标微调速率，用线性插值重采样；积压超过 4 倍目标就直接跳帧
- 界面是 X11 子窗口 + tiny-skia 软件绘制（MIT-SHM 上屏），没有 GL 依赖，Hyprland / XWayland 下都能嵌进宿主窗口

## 已知限制

- 只支持 X11 窗口（Wayland 原生窗口没做，REAPER 本身走 XWayland 所以没问题）
- 只抓系统默认输出。想单独抓某个程序，就把它指到默认输出设备
- 只发 CLAP，不打算做 VST3 / LV2 —— `Cargo.toml` 里的 `rlib` 只是给 examples / 测试用，不是插件格式
- 只在 REAPER + PipeWire 1.6 / Hyprland 上验证过，其他 CLAP 宿主理论上也能用
- 没有 Windows / macOS 支持（这两边本来就不需要它）

## 开发

```bash
# 不开 DAW，直接验证 PipeWire 这一侧抓没抓到声音
cargo run --release --example probe
cargo run --release --example probe -- Loopback      # 抓指定 sink

# 重新渲染 README 的预览图（第二个参数是 ZOOM 档位 0..5）
cargo run --release --example preview -- docs/preview.png 1

# 调试：打印 CLAP 回调；或让插件忽略抓音、直接吐 440Hz（用来判断宿主有没有采纳输出）
TAP_DEBUG=1 reaper
TAP_DEBUG_TONE=1 reaper
```

## License

GPL-3.0-or-later
