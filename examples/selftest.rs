//! 自测：验证「不会抓到自己」——也就是结构上不可能啸叫。
//!
//! 它在**同一个进程**里开一个播放流放 440 Hz（送到虚拟 sink，不会吵到人），
//! 同时用 TAP 的捕获逻辑去抓应用音频。因为捕获会排除本进程，
//! 所以应当一帧都抓不到。抓到东西就说明排除失效，会有回授风险。
//!
//! ```bash
//! cargo run --release --example selftest
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pipewire as pw;
use pipewire::properties::properties;
use tap::capture::Capture;
use tap::ring::Frame;

fn main() {
    let target = std::env::args().nth(1).unwrap_or_else(|| "Loopback".to_string());
    let cap = Capture::new();
    cap.start();
    std::thread::sleep(Duration::from_millis(600));

    let quit = Arc::new(AtomicBool::new(false));
    let player = {
        let quit = quit.clone();
        let target = target.clone();
        std::thread::spawn(move || play_tone(&target, quit))
    };
    std::thread::sleep(Duration::from_millis(400));

    let ring = cap.ring();
    let mut f = Frame::default();
    let (mut n, mut peak) = (0u64, 0f32);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(2) {
        while ring.read_one(&mut f) {
            n += 1;
            peak = peak.max(f.l.abs()).max(f.r.abs());
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    quit.store(true, Ordering::Relaxed);
    let _ = player.join();

    println!("自己放音 2 秒，捕获到 frames={n} peak={peak:.4}");
    if n == 0 {
        println!("PASS: 本进程的播放流被排除了，不会有回授");
    } else {
        println!("FAIL: 抓到了自己的输出，会啸叫！");
        println!("（如果此时有别的声音在放，先关掉别的播放器再跑一次——");
        println!("  这个自测只能保证「自己的播放流」没有被抓，抓不到别人的东西就没法判断）");
        std::process::exit(1);
    }
}

fn play_tone(target: &str, quit: Arc<AtomicBool>) {
    pw::init();
    let Ok(mainloop) = pw::main_loop::MainLoopRc::new(None) else {
        return;
    };
    let Ok(context) = pw::context::ContextRc::new(&mainloop, None) else {
        return;
    };
    let Ok(core) = context.connect_rc(None) else { return };

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::NODE_NAME => "tap-selftest-tone",
        "node.description" => "TAP selftest tone",
    };
    if !target.is_empty() {
        props.insert("target.object", target.to_string());
    }
    let Ok(stream) = pw::stream::StreamRc::new(core.clone(), "tap-selftest", props) else {
        return;
    };

    let phase = Arc::new(std::sync::Mutex::new(0.0f64));
    let _listener = stream
        .add_local_listener_with_user_data(())
        .process({
            let phase = phase.clone();
            move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(d) = datas.first_mut() else { return };
                let n = d.chunk().size() as usize / 4;
                let Some(bytes) = d.data() else { return };
                let out: &mut [f32] =
                    unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast(), n) };
                let mut ph = phase.lock().unwrap();
                for (i, s) in out.iter_mut().enumerate() {
                    let ch = i % 2;
                    if ch == 0 {
                        *ph += 1.0 / 48000.0;
                    }
                    *s = (0.3 * (std::f64::consts::TAU * 440.0 * *ph).sin()) as f32;
                    if *ph > 1.0 {
                        *ph -= 1.0;
                    }
                }
            }
        })
        .register();

    let mut audio_info = pw::spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(pw::spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(48000);
    audio_info.set_channels(2);
    let obj = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let Ok(values) = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    ) else {
        return;
    };
    let vals = values.0.into_inner();
    let mut params = [pw::spa::pod::Pod::from_bytes(&vals).unwrap()];
    if stream
        .connect(
            pw::spa::utils::Direction::Output,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .is_err()
    {
        return;
    }

    while !quit.load(Ordering::Relaxed) {
        mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(20)));
    }
}
