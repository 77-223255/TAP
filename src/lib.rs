//! TAP —— 把电脑里正在播放的声音，像麦克风一样接到 DAW 轨道上。
//!
//! 一个 CLAP 插件：PipeWire 后台线程抓取**系统默认输出**的 monitor，
//! 环形缓冲交给音频线程，音频线程把音频吐到插件输出端口。
//! DAW 里把轨道录音模式设成 `Record: output (stereo)` 就能直接录。
//!
//! 只做一件事：取音。不选设备、不写文件、不做编辑，那些交给宿主。

pub mod capture;
pub mod ctl;
pub mod gui;
pub mod plugin;
pub mod ring;
