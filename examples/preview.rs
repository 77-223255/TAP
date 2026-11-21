//! 把界面渲染成 PNG，用来生成 README 的预览图：
//!
//! ```bash
//! cargo run --release --example preview -- docs/preview.png [zoom 0..5]
//! ```

use tap::gui::{render_png, View};

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "docs/preview.png".to_string());
    let zoom = std::env::args()
        .nth(2)
        .and_then(|z| z.parse::<i32>().ok())
        .unwrap_or(1)
        .clamp(0, 5);

    let v = View {
        running: true,
        gain_db: -1.5,
        // 线性峰值，画出来差不多 -12 dBFS
        meters: [0.30, 0.26],
        holds: [0.36, 0.31],
        led: 0.62,
        buffer_secs: 0.061,
        host_rate: 48000,
        card_rate: 48000,
        zoom,
        error: None,
        dropped: 0,
        apps: vec!["Chromium".to_string(), "Spotify".to_string()],
    };

    match render_png(std::path::Path::new(&out), &v) {
        Some(()) => println!("wrote {out}"),
        None => {
            eprintln!("render failed");
            std::process::exit(1);
        }
    }
}
