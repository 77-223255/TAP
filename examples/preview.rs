//! 渲染预览图：
//!
//! ```bash
//! cargo run --release --example preview -- docs/preview.png [zoom 0..5] [menu]
//! ```
//!
//! 第三个参数给 `menu` 就把右键菜单也画上。

use tap::ctl::Ctl;
use tap::gui::{render_png, render_png_with_menu, View};

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "docs/preview.png".to_string());
    let zoom = std::env::args()
        .nth(2)
        .and_then(|z| z.parse::<i32>().ok())
        .unwrap_or(1)
        .clamp(0, 5);
    let with_menu = std::env::args().nth(3).as_deref() == Some("menu");

    let v = View {
        enabled: true,
        gate: true,
        recording: true,
        running: true,
        gain_db: -1.5,
        // 线性峰值，画出来差不多 -12 dBFS
        meters: [0.28, 0.24],
        holds: [0.34, 0.29],
        zoom,
        active: true,
        error: None,
    };

    let ctl = Ctl::new();
    ctl.set_enabled(true);
    ctl.set_gate(true);
    ctl.set_zoom(zoom);

    let r = if with_menu {
        render_png_with_menu(std::path::Path::new(&out), &v, &ctl, 300.0, 60.0)
    } else {
        render_png(std::path::Path::new(&out), &v)
    };
    match r {
        Some(()) => println!("wrote {out}"),
        None => {
            eprintln!("render failed");
            std::process::exit(1);
        }
    }
}
