# TAP

System audio into a DAW track, like a microphone. **PipeWire / CLAP / Linux**.

![TAP](docs/preview.png)

## Use

1. Insert **TAP** on a track → right-click the panel → tick **`Capture`** (off by default)
2. Set the track's record mode to **`Record: output (stereo)`**
3. Hit record

**Record without hearing it** (like Live): track `Route` → untick `Master send`.
Recording still works — the record tap sits before the send to the master.

Knob is ±24 dB (double-click resets). The right-click menu holds `Capture` and a
`Zoom ▸` submenu with 50–300 %.

Full REAPER setup, export and troubleshooting → **[docs/reaper.md](docs/reaper.md)**

## Install

```bash
cargo build --release && ./install.sh   # → ~/.clap/tap.clap
```

Then `Options → Preferences → Plug-ins → CLAP → Re-scan` in REAPER.

Requires `rust`, `pipewire`, `libx11` and any CJK font at runtime.

## How it works

Capturing a sink monitor feeds the host's own output back into the plugin:
plugin output → sound card → monitor → plugin input is a loop with unity gain,
so it howls within seconds. So TAP does the opposite:

> every application playback stream (`media.class = Stream/Output/Audio`) is
> linked into the plugin's input ports with `link-factory`, and everything that
> belongs to the host process itself is excluded — a feedback loop is
> structurally impossible.

Output = input + capture (pure passthrough when `Capture` is off).

## Develop

```bash
cargo run --release --example probe        # is PipeWire delivering audio?
cargo run --release --example selftest     # does it ever capture itself? (no feedback)
cargo run --release --example preview -- docs/preview.png 2 menu
```

GPL-3.0-or-later
