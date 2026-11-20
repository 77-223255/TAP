# NeiLu 内录

**把电脑里正在播放的声音，像麦克风一样录进你的 DAW 轨道。**

一个 Linux / PipeWire 专用的 CLAP 插件。装上、点一下开始，Reaper 里那条轨道就开始长音频——不用虚拟声卡、不用 `parecord`、不用来回搬文件。

![screenshot](docs/preview.png)

## 为什么需要它

USB 声卡（比如 Steinberg UR12）在 Linux 上没有硬件 loopback，`dspMixFx` 那套是 Windows 驱动的功能。想在 Linux 里内录，通常得靠命令行把 PipeWire 的 monitor 录成 wav 再拖回工程。

NeiLu 把这件事搬进插件里：它自己作为 PipeWire 客户端去抓任意 Sink 的 monitor，把音频吐到插件的输出端口，于是「录进轨道」就变成了普通的录音动作。

## 用法（Reaper）

1. 在任意轨道上插入 **NeiLu 内录**
2. 把该轨道的录音模式改成 **Record: output (stereo)**，然后按下录音键
3. 点界面上的 **开始录制**（默认已经跟随系统默认输出，所以通常什么都不用选）

想采特定的程序（浏览器、播放器）：把它单独指到那个设备，或者在下拉里选具体设备。

想调音量：拖 **增益** 滑条。看信号：右边两条电平表 + 缓冲条。

## 安装

```bash
cargo build --release
./install.sh          # 装到 ~/.clap/neilu.clap
```

然后在 Reaper 里 `Options → Preferences → Plug-ins → CLAP → Re-scan`，FX 浏览器搜 “NeiLu”。

依赖（Arch）：`rust`、`pipewire`、`libx11`、`noto-fonts`（或任何带中文的字体）。运行时不需要额外装任何东西。

## 工作原理

```
PipeWire 线程 ──pw_stream(monitor)──▶ 无锁环形缓冲 ──▶ 音频线程 ──▶ 插件输出端口
                                            ▲
                                    按填充量微调播放速率
                                    （和 DAW 时钟的漂移补偿）
```

- 捕获跑在 PipeWire 自己的实时线程里，只做一件事：把交错立体声写进环形缓冲，不分配内存、不加锁
- 音频线程按缓冲填充量做 ±2% 的速率微调，用线性插值重采样，避免两端时钟长期漂移积累成爆音
- 界面是 X11 子窗口 + tiny-skia 软件绘制，没有 GL 依赖，Hyprland / XWayland 下都能嵌进宿主窗口
- PipeWire 线程、音频线程、界面线程之间只共享原子量和一把小互斥锁

## 已知限制

- 只支持 X11 窗口（Wayland 原生窗口还没做，Reaper 本身走 XWayland 所以没问题）
- 采样率跟随宿主的 `activate`，PipeWire 那一侧按同一采样率打开，48k 最省事
- 只在 Reaper + PipeWire 1.6 / Hyprland 上验证过，其他 CLAP 宿主理论上也能用
- 没有 Windows / macOS 支持（这两边本来就不需要它）

## License

GPL-3.0-or-later
