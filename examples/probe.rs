//! 命令行探针：不开 DAW，直接验证 PipeWire 这一侧有没有真的抓到系统输出。
//!
//! ```bash
//! cargo run --release --example probe
//! ```

use std::thread::sleep;
use std::time::{Duration, Instant};

use tap::capture::Capture;
use tap::ring::Frame;

fn main() {
    let cap = Capture::new();
    cap.start();
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
    while t0.elapsed() < Duration::from_secs(3) {
        while ring.read_one(&mut f) {
            peak = peak.max(f.l.abs()).max(f.r.abs());
            acc += (f.l * f.l + f.r * f.r) as f64;
            n += 2;
        }
        sleep(Duration::from_millis(5));
    }
    let rms = (acc / n.max(1) as f64).sqrt();
    println!(
        "frames={n} peak={peak:.4} rms={rms:.4} dropped={}",
        cap.dropped_frames()
    );
    if n == 0 {
        println!("没有数据：检查 PipeWire 是否在跑、以及是不是真的在放声音");
        std::process::exit(2);
    }
}
