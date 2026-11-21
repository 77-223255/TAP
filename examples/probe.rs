//! 命令行探针：不开 DAW，直接验证 PipeWire 这一侧有没有真的抓到声音。
//!
//! ```bash
//! # 跟随系统默认输出，抓 2 秒，打印峰值/RMS
//! cargo run --release --example probe
//!
//! # 抓指定 sink（node.name，pw-dump 里能看到）
//! cargo run --release --example probe -- Loopback
//! ```

use std::thread::sleep;
use std::time::{Duration, Instant};

use tap::capture::Capture;
use tap::ring::Frame;

fn main() {
    let sink = std::env::args().nth(1);
    let cap = Capture::new();
    sleep(Duration::from_millis(400));

    let sinks = cap.sinks();
    println!("sinks ({}):", sinks.len());
    for s in &sinks {
        println!("  {:<62} {}", s.name, s.label);
    }
    match &sink {
        Some(s) => println!("target: {s}"),
        None => println!("target: <系统默认输出>"),
    }

    cap.start(sink.as_deref());
    sleep(Duration::from_millis(700));
    println!(
        "running={} card_rate={} error={:?}",
        cap.is_running(),
        cap.rate(),
        cap.error()
    );

    let ring = cap.ring();
    let mut f = Frame::default();
    let mut peak = 0f32;
    let mut acc = 0f64;
    let mut n = 0u64;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(2) {
        while ring.read_one(&mut f) {
            peak = peak.max(f.l.abs()).max(f.r.abs());
            acc += (f.l * f.l + f.r * f.r) as f64;
            n += 2;
        }
        sleep(Duration::from_millis(5));
    }
    let rms = (acc / n.max(1) as f64).sqrt();
    println!("frames={n} peak={peak:.4} rms={rms:.4} dropped={}", cap.dropped_frames());
    if n == 0 {
        println!("没有数据：检查 PipeWire 是否在跑、以及是不是真的在放声音");
        std::process::exit(2);
    }
}
