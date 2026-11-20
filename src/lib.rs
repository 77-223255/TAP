//! NeiLu —— 把电脑里的声音，像麦克风一样录进 Reaper。
//!
//! 一个 CLAP 插件：PipeWire 后台线程抓取任意 Sink 的 monitor，
//! 环形缓冲交给音频线程，音频线程吐到插件输出端口。
//! DAW 里把轨道录音模式设成 `Record: output (stereo)` 就能直接录。

pub mod capture;
pub mod ctl;
pub mod gui;
pub mod plugin;
pub mod ring;
