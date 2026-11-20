//! 插件与 GUI 线程之间唯一的共享状态：全是原子量和普通互斥锁，
//! 音频线程只碰原子量。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::capture::SinkInfo;

pub fn f32_to_bits(v: f32) -> u32 {
    v.to_bits()
}

pub fn bits_to_f32(v: u32) -> f32 {
    f32::from_bits(v)
}

pub struct Ctl {
    pub meter: [AtomicU32; 2],
    pub meter_hold: [AtomicU32; 2],
    pub hold_age: [AtomicU32; 2],
    pub fill: AtomicU32,
    pub capacity: AtomicU32,
    pub running: AtomicBool,
    pub gain_db: AtomicU32,
    pub source: AtomicI32,
    pub sr: AtomicU32,
    pub dropped: AtomicU64,
    pub received: AtomicU64,
    pub sinks: Mutex<Vec<SinkInfo>>,
    pub want_start: Mutex<Option<Option<String>>>,
    pub want_stop: AtomicBool,
}

impl Ctl {
    pub fn new() -> Self {
        Self {
            meter: [AtomicU32::new(0), AtomicU32::new(0)],
            meter_hold: [AtomicU32::new(0), AtomicU32::new(0)],
            hold_age: [AtomicU32::new(0), AtomicU32::new(0)],
            fill: AtomicU32::new(0),
            capacity: AtomicU32::new(32768),
            running: AtomicBool::new(false),
            gain_db: AtomicU32::new(0f32.to_bits()),
            source: AtomicI32::new(0),
            sr: AtomicU32::new(48000),
            dropped: AtomicU64::new(0),
            received: AtomicU64::new(0),
            sinks: Mutex::new(Vec::new()),
            want_start: Mutex::new(None),
            want_stop: AtomicBool::new(false),
        }
    }

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

    pub fn gain_db(&self) -> f32 {
        bits_to_f32(self.gain_db.load(Ordering::Relaxed))
    }
    pub fn set_gain_db(&self, v: f32) {
        self.gain_db.store(f32_to_bits(v), Ordering::Relaxed);
    }

    pub fn source(&self) -> i32 {
        self.source.load(Ordering::Relaxed)
    }
    pub fn set_source(&self, v: i32) {
        self.source.store(v, Ordering::Relaxed);
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
    pub fn set_running(&self, v: bool) {
        self.running.store(v, Ordering::Relaxed);
    }

    pub fn sr(&self) -> u32 {
        self.sr.load(Ordering::Relaxed)
    }
    pub fn fill(&self) -> u32 {
        self.fill.load(Ordering::Relaxed)
    }
    pub fn capacity(&self) -> u32 {
        self.capacity.load(Ordering::Relaxed)
    }
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn sinks(&self) -> Vec<SinkInfo> {
        self.sinks.lock().map(|v| v.clone()).unwrap_or_default()
    }
    pub fn set_sinks(&self, v: Vec<SinkInfo>) {
        if let Ok(mut g) = self.sinks.lock() {
            if *g != v {
                *g = v;
            }
        }
    }

    /// GUI 请求开始捕获（None = 跟随系统默认输出）
    pub fn request_start(&self, sink: Option<String>) {
        if let Ok(mut g) = self.want_start.lock() {
            *g = Some(sink);
        }
        self.want_stop.store(false, Ordering::Relaxed);
    }
    pub fn request_stop(&self) {
        self.want_stop.store(true, Ordering::Relaxed);
    }
    pub fn take_start(&self) -> Option<Option<String>> {
        self.want_start.lock().ok().and_then(|mut g| g.take())
    }
    pub fn take_stop(&self) -> bool {
        self.want_stop.swap(false, Ordering::Relaxed)
    }
}

impl Default for Ctl {
    fn default() -> Self {
        Self::new()
    }
}
