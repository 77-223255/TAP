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

## 为什么这么设计（重要）

最直觉的做法是「抓 sink 的 monitor」。**那样一定会啸叫**：monitor 里包含宿主自己的输出，
于是 插件输出 → 声卡 → monitor → 插件输入 形成增益为 1 的环路，几秒内就炸。

Ardour 论坛和 OBS 社区对这件事的结论一致：抓 monitor 就必须断开宿主输出、
或者另建虚拟 sink、或者靠“关监听 + 静音轨道”来躲。都不适合「插上就用」。

所以 TAP 走 OBS 的 *Application Audio Capture* 那条路：

> **不抓 monitor。把每个应用的播放流节点直接连进插件的输入口，并显式排除本进程（宿主自己）。**

于是宿主的输出永远不会被自己抓回来 —— **结构上不可能成环**，
不需要虚拟声卡、不需要关监听、不需要静音轨道，也可以边录边听。

## 用法（REAPER 为例）

1. 在轨道上插入 **TAP**（插上就开始抓系统默认输出）
2. 轨道录音模式：右键录音按钮 / 右键轨道 → `Record: output (stereo)`，
   **再点一次选择子模式**：`post fader` / `post-fx pre-fader` / `pre fx`
   （想没了推子影响就选 `post-fx pre-fader`）
3. 按下录音键。轨道上就会出现捕获到的音频

底栏 `APPS · …` 会列出当前正在被捕获的应用；空着说明没有应用在发声。

**录音时别开轨道的监听。** 源本来就在响（你能听见浏览器/播放器），
监听只会再叠一份晚了约 50ms 的副本，听感就像回声。

### OUTPUT 开关：REC ONLY / ALWAYS

- **REC ONLY（默认）**：只有 transport 处于录音状态时才往外输出；不录的时候输出静音。
  这样工程里重放时，插件不会再把实时声音叠到已经录好的片段上
  （那种「宿主又放了一遍」的怪感就是它造成的），电平表也会跟着静下来。
- **ALWAYS**：一直输出捕获到的音频。适合把系统声音当实时输入用（走 FX、做现场处理）。

录音时状态灯应当显示 **REC**；如果显示 **ARMED**，说明你的宿主没有在 CLAP transport
里报录音标志（REAPER 需要 ≥ 7.66，7.66 的更新日志里写了 “CLAP: fix transport flags
when REAPER is recording”）。这种情况把 OUTPUT 切到 ALWAYS 即可。

想单独抓某个程序（浏览器、播放器）：把它指到某个输出设备，然后在列表里选那个设备。

### 状态灯

| 灯 | 含义 |
|---|---|
| `REC` | 宿主在录音，正在往外吐音频（灯亮度跟电平走） |
| `ARMED` | 没在录音，输出静音（REC ONLY 模式） |
| `LIVE` | ALWAYS 模式，正在实时输出 |
| `ERROR` | 抓音出错，看底栏错误文本 |

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
应用播放流 ──link-factory──▶ TAP 的输入口 ──▶ 无锁环形缓冲 ──▶ 音频线程 ──▶ 插件输出端口
（宿主的流被排除）                                    ▲
                                              按填充量微调播放速率
                                             （和 DAW 时钟的漂移补偿）
```

- 枚举 registry 里的节点：`media.class == "Stream/Output/Audio"` 就是应用播放流；
  节点的 props 里常常没有 pid，要顺着 `client.id` 去 Client 对象上找
  `application.process.id`（兜底 `pipewire.sec.pid`），和 `std::process::id()` 比较来排除宿主
- 端口对端口用 `link-factory` 连接（`link.output.port` / `link.input.port`），
  单声道应用会同时接左右两路；节点/端口增删由 registry 事件驱动，250ms 兜底重扫一次
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
- 抓的是**所有**应用播放流（除宿主自己），暂时不能只挑某一个应用
- 录音门控依赖 CLAP transport 的录音标志，REAPER 要 ≥ 7.66
- 有些声音不走应用播放流（极少数直接写给 sink 的东西），抓不到
- 只发 CLAP，不打算做 VST3 / LV2 —— `Cargo.toml` 里的 `rlib` 只是给 examples / 测试用，不是插件格式
- 只在 REAPER + PipeWire 1.6 / Hyprland 上验证过，其他 CLAP 宿主理论上也能用
- 没有 Windows / macOS 支持（这两边本来就不需要它）

## 开发

```bash
# 不开 DAW，直接验证 PipeWire 这一侧抓没抓到声音
cargo run --release --example probe

# 自测：本进程自己放音 2 秒，应当一帧都抓不到（证明宿主输出被排除 → 不会啸叫）
cargo run --release --example selftest

# 重新渲染 README 的预览图（第二个参数是 ZOOM 档位 0..5）
cargo run --release --example preview -- docs/preview.png 1

# 调试：打印 CLAP 回调；或让插件忽略抓音、直接吐 440Hz（用来判断宿主有没有采纳输出）
TAP_DEBUG=1 reaper
TAP_DEBUG_TONE=1 reaper
```

## License

GPL-3.0-or-later
