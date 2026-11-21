//! 把界面渲染成 PNG，用来生成 README 的预览图：
//!
//! ```bash
//! cargo run --release --example preview -- docs/preview.png
//! ```

use tap::gui::{render_png, View};

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "docs/preview.png".to_string());

    let v = View {
        sinks: vec![
            "Steinberg UR12".to_string(),
            "G24H2".to_string(),
            "HDMI 3".to_string(),
            "Speaker".to_string(),
            "Loopback".to_string(),
        ],
        source: 1,
        running: true,
        gain_db: -1.5,
        // 线性峰值，画出来差不多 -12 dBFS
        meters: [0.30, 0.26],
        holds: [0.36, 0.31],
        buffer_secs: 0.061,
        host_rate: 48000,
        card_rate: 48000,
        zoom: std::env::args()
            .nth(2)
            .and_then(|z| z.parse::<i32>().ok())
            .unwrap_or(1)
            .clamp(0, 5),
        error: None,
        dropped: 0,
    };

    match render_png(std::path::Path::new(&out), &v) {
        Some(()) => println!("wrote {out}"),
        None => {
            eprintln!("render failed");
            std::process::exit(1);
        }
    }
}
