//! 插件、GUI、音频线程之间唯一的共享状态。
//!
//! 音频线程只碰原子量；`Mutex` 只出现在错误文本这种与实时无关的地方。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

/// 界面缩放档位：50 / 100 / 150 / 200 / 250 / 300 %
pub const ZOOMS: [f32; 6] = [0.5, 1.0, 1.5, 2.0, 2.5, 3.0];

pub fn zoom_scale(idx: i32) -> f32 {
    ZOOMS[idx.clamp(0, ZOOMS.len() as i32 - 1) as usize]
}

/// 最接近某个缩放系数的档位下标
pub fn zoom_index(scale: f32) -> i32 {
    let mut best = 0i32;
    let mut err = f32::MAX;
    for (i, z) in ZOOMS.iter().enumerate() {
        let e = (z - scale).abs();
        if e < err {
            err = e;
            best = i as i32;
        }
    }
    best
}

pub fn f32_to_bits(v: f32) -> u32 {
    v.to_bits()
}

pub fn bits_to_f32(v: u32) -> f32 {
    f32::from_bits(v)
}

pub struct Ctl {
    meter: [AtomicU32; 2],
    meter_hold: [AtomicU32; 2],
    fill: AtomicU32,
    capacity: AtomicU32,
    running: AtomicBool,
    gain_db: AtomicU32,
    sr: AtomicU32,
    dropped: AtomicU64,
    error: Mutex<String>,
    zoom: AtomicI32,
    zoom_dirty: AtomicBool,
    /// 0 = 只在录音时输出（默认），1 = 一直输出
    mode: AtomicI32,
    recording: AtomicBool,
}

impl Ctl {
    pub fn new() -> Self {
        Self {
            meter: [AtomicU32::new(0), AtomicU32::new(0)],
            meter_hold: [AtomicU32::new(0), AtomicU32::new(0)],
            fill: AtomicU32::new(0),
            capacity: AtomicU32::new(32768),
            running: AtomicBool::new(false),
            gain_db: AtomicU32::new(0f32.to_bits()),
            sr: AtomicU32::new(48000),
            dropped: AtomicU64::new(0),
            error: Mutex::new(String::new()),
            zoom: AtomicI32::new(1),
            zoom_dirty: AtomicBool::new(false),
            mode: AtomicI32::new(0),
            recording: AtomicBool::new(false),
        }
    }

    // ---- 电平
    pub fn meter(&self, ch: usize) -> f32 {
        bits_to_f32(self.meter[ch].load(Ordering::Relaxed))
    }
    pub fn hold(&self, ch: usize) -> f32 {
        bits_to_f32(self.meter_hold[ch].load(Ordering::Relaxed))
    }
    pub fn set_meter(&self, ch: usize, v: f32) {
        self.meter[ch].store(f32_to_bits(v), Ordering::Relaxed);
    }
    pub fn set_hold(&self, ch: usize, v: f32) {
        self.meter_hold[ch].store(f32_to_bits(v), Ordering::Relaxed);
    }

    // ---- 增益
    pub fn gain_db(&self) -> f32 {
        bits_to_f32(self.gain_db.load(Ordering::Relaxed))
    }
    pub fn set_gain_db(&self, v: f32) {
        self.gain_db.store(f32_to_bits(v), Ordering::Relaxed);
    }

    // ---- 运行状态
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
    pub fn set_running(&self, v: bool) {
        self.running.store(v, Ordering::Relaxed);
    }

    // ---- 采样率
    pub fn sr(&self) -> u32 {
        self.sr.load(Ordering::Relaxed)
    }
    pub fn set_sr(&self, v: u32) {
        self.sr.store(v, Ordering::Relaxed);
    }

    // ---- 缓冲
    pub fn fill(&self) -> u32 {
        self.fill.load(Ordering::Relaxed)
    }
    pub fn set_fill(&self, v: u32) {
        self.fill.store(v, Ordering::Relaxed);
    }
    pub fn capacity(&self) -> u32 {
        self.capacity.load(Ordering::Relaxed)
    }
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
    pub fn set_dropped(&self, v: u64) {
        self.dropped.store(v, Ordering::Relaxed);
    }

    // ---- 错误
    pub fn error(&self) -> String {
        self.error.lock().map(|s| s.clone()).unwrap_or_default()
    }
    pub fn set_error(&self, v: String) {
        if let Ok(mut g) = self.error.lock() {
            if *g != v {
                *g = v;
            }
        }
    }

    // ---- 输出模式 / 录音状态
    pub fn mode(&self) -> i32 {
        self.mode.load(Ordering::Relaxed)
    }
    pub fn set_mode(&self, v: i32) {
        self.mode.store(v.clamp(0, 1), Ordering::Relaxed);
    }
    pub fn recording(&self) -> bool {
        self.recording.load(Ordering::Relaxed)
    }
    pub fn set_recording(&self, v: bool) {
        self.recording.store(v, Ordering::Relaxed);
    }

    // ---- 缩放
    pub fn zoom(&self) -> i32 {
        self.zoom.load(Ordering::Relaxed)
    }
    pub fn zoom_scale(&self) -> f32 {
        zoom_scale(self.zoom())
    }
    /// 用户/宿主改了缩放：标记需要向宿主申请新窗口尺寸
    pub fn set_zoom(&self, idx: i32) {
        let idx = idx.clamp(0, ZOOMS.len() as i32 - 1);
        if self.zoom.swap(idx, Ordering::Relaxed) != idx {
            self.zoom_dirty.store(true, Ordering::Relaxed);
        }
    }
    /// 宿主自己定了尺寸：同步档位但不要再回敬一次 resize
    pub fn set_zoom_quiet(&self, idx: i32) {
        self.zoom
            .store(idx.clamp(0, ZOOMS.len() as i32 - 1), Ordering::Relaxed);
    }
    pub fn take_zoom_dirty(&self) -> bool {
        self.zoom_dirty.swap(false, Ordering::Relaxed)
    }
}

impl Default for Ctl {
    fn default() -> Self {
        Self::new()
    }
}
