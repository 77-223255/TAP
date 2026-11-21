# TAP in REAPER

## Set it up once

### 1. Record without hearing it

The plugin cannot touch the host's routing, so this step lives in REAPER:

1. Click the track's **`Route`** button
2. Untick **`Master send`**

Nothing from that track reaches the master any more: while recording you only
hear the source itself, and `Record: output` still captures everything because
the record tap sits before the send to the master.

Tick it again when you want to listen to what you recorded.

### 2. Record mode

Right-click the track's record button (or the track → `Record: output`):

- Pick **`Record: output (stereo)`**
- **Click again** and pick a sub-mode:
  - `post fader` — follows the fader and the mute
  - `post-fx pre-fader` — recommended: after the plugin, before the fader
  - `pre fx` — before the plugin

### 3. The plugin

Insert **TAP** → right-click the panel → tick **`Capture`** (it ships off).

The level ring moving means it is capturing. `Zoom` is in the same menu.

## Record

Press record; the audio lands on the track.

- **Do not enable input monitoring**: the source is already audible, monitoring
  only adds a second copy a few dozen milliseconds late
- If you hear everything twice, `Master send` is probably still on
- Every application's playback is captured (the host is excluded), so browsers,
  players and games all end up in the recording

## Export

**Render**

1. Select the track
2. `File → Render…`
3. `Source` = **`Selected tracks (stems)`**, `Bounds` = `Entire project` or
   `Time selection`, `Output` = `WAV` / `24 bit`
4. Pick a directory and a file name → **`Render 1 file`**

Rendering is offline and the plugin is a passthrough whenever `Capture` adds
nothing to it, so nothing that happens to be playing leaks into the file.

**Stem in place**

Track → `Render/freeze tracks` → `Render tracks to stereo stem tracks (and mute
originals)`. A `[stem]` track appears with the audio; the file itself lives in
the project's Render/Media folder.

**A single item**

Right-click the item → `Apply track/take FX to items as new take`, then `Glue
items`.

## Troubleshooting

| Symptom | Cause |
|---|---|
| The level ring never moves | `Capture` is off, or no application is playing |
| Level moves, the track stays silent | record mode is `input` instead of `Record: output` |
| The microphone got recorded | record mode is `input (audio or MIDI)` |
| Everything is heard twice | `Master send` is still on, or input monitoring is enabled |
| A recorded item plays nothing | the track's `Master send` is off (turn it on to listen) |
| An orange message on the panel | PipeWire could not be reached; the text says why |

Debugging:

```bash
TAP_DEBUG=1 reaper        # CLAP callbacks and capture errors on stderr
TAP_DEBUG_TONE=1 reaper   # ignore the capture, emit 440 Hz (is the host using our output?)
```

## Why not the monitor

A sink monitor contains the host's own output. Capturing it closes a loop with
unity gain (plugin output → sound card → monitor → plugin input) that howls
within seconds. TAP therefore captures the playback streams of the applications
themselves and excludes its own process, which makes that loop impossible.
