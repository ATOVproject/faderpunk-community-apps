//! Dozen Ctrl — 4×3 latched MIDI CC banks for one synth on one channel.
//!
//! Device enum loads ★ suggested CCs into M/A/T slots; full per-device maps
//! live in the main Faderpunk repo (`device-cc-catalog.json`).

pub const DEVICE_COUNT: usize = 23;

pub const DEVICE_NAMES: &[&str] = &[
    "Hydrasynth",
    "Blofeld",
    "LXR-02",
    "Tempera",
    "Vector",
    "Minitaur",
    "P-6",
    "Drum 3P",
    "Streichfett",
    "Polyphrase",
    "M-Vave FM1",
    "Digitone",
    "Digitakt",
    "Syntakt",
    "DeepMind 12",
    "Minilogue XD",
    "MiniFreak",
    "Take 5",
    "Bass Station II",
    "S-1",
    "Iridium",
    "Subsequent 37",
    "microKORG",
];

/// Default CC numbers for slots M1–M4, A1–A4, T1–T4 per device.
pub const DEFAULT_CCS: [[u8; 12]; DEVICE_COUNT] = [
    [1, 14, 15, 29, 30, 31, 33, 34, 35, 36, 37, 39],
    [1, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25],
    [21, 22, 23, 24, 25, 26, 27, 30, 31, 32, 33, 34],
    [17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 53, 54],
    [12, 18, 20, 21, 22, 23, 24, 26, 27, 29, 1, 2],
    [82, 85, 86, 87, 89, 112, 5, 65, 70, 71, 72, 73],
    [12, 24, 26, 71, 74, 78, 85, 86, 89, 90, 3, 7],
    [12, 13, 14, 15, 16, 17, 30, 31, 44, 45, 46, 47],
    [1, 76, 91, 92, 93, 94, 64, 70, 71, 72, 73, 74],
    [2, 3, 1, 4, 5, 6, 7, 8, 9, 10, 11, 13],
    [1, 11, 70, 71, 73, 74, 75, 79, 91, 93, 94, 95],
    [9, 12, 13, 14, 19, 21, 22, 23, 24, 25, 26, 27],
    [2, 13, 14, 24, 25, 27, 28, 29, 70, 71, 73, 74],
    [13, 14, 15, 16, 18, 19, 20, 21, 22, 23, 24, 25],
    [12, 13, 31, 46, 47, 48, 49, 58, 59, 60, 61, 114],
    [24, 26, 27, 28, 29, 41, 43, 56, 57, 58, 59, 83],
    [1, 24, 68, 69, 74, 76, 77, 80, 81, 83, 94, 5],
    [1, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26],
    [16, 17, 18, 19, 28, 31, 71, 72, 73, 76, 77, 78],
    [1, 3, 12, 13, 17, 24, 25, 26, 27, 71, 74, 79],
    [9, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 93],
    [1, 3, 11, 16, 18, 19, 21, 22, 23, 24, 26, 27],
    [12, 13, 23, 24, 26, 27, 71, 74, 76, 79, 82, 83],
];

use embassy_futures::{
    join::{join, join5},
    select::{select, select3},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use heapless::Vec;
use libfp::{
    ext::FromValue,
    latch::LatchLayer,
    utils::{midi_gate, split_unsigned_value},
    AppIcon, Brightness, Color, Config, MidiCc, MidiChannel, MidiOut, Param, Value,
    APP_MAX_PARAMS,
};
use serde::{Deserialize, Serialize};
use smart_leds::RGB8;

use crate::app::{App, AppParams, AppStorage, Led, ManagedStorage, ParamStore, SceneEvent};

pub const CHANNELS: usize = 4;
pub const PARAMS: usize = 16;

const RESET_FADER_DEADBAND: u16 = 32;

pub static CONFIG: Config<PARAMS> = Config::new(
    "Dozen Ctrl",
    "4x3 latched MIDI CC expander for one device",
    Color::Cyan,
    AppIcon::Fader,
)
.add_param(Param::Enum {
    name: "Device",
    variants: DEVICE_NAMES,
})
.add_param(Param::MidiChannel {
    name: "MIDI Channel",
})
.add_param(Param::MidiCc { name: "M1" })
.add_param(Param::MidiCc { name: "M2" })
.add_param(Param::MidiCc { name: "M3" })
.add_param(Param::MidiCc { name: "M4" })
.add_param(Param::MidiCc { name: "A1" })
.add_param(Param::MidiCc { name: "A2" })
.add_param(Param::MidiCc { name: "A3" })
.add_param(Param::MidiCc { name: "A4" })
.add_param(Param::MidiCc { name: "T1" })
.add_param(Param::MidiCc { name: "T2" })
.add_param(Param::MidiCc { name: "T3" })
.add_param(Param::MidiCc { name: "T4" })
.add_param(Param::Color {
    name: "Color",
    variants: &[
        Color::Blue,
        Color::Green,
        Color::Rose,
        Color::Orange,
        Color::Cyan,
        Color::Pink,
        Color::Violet,
        Color::Yellow,
    ],
})
.add_param(Param::MidiOut);

pub struct Params {
    device: usize,
    midi_channel: MidiChannel,
    slots: [MidiCc; 12],
    color: Color,
    midi_out: MidiOut,
}

impl Params {
    fn cc_for_layer(&self, layer: LatchLayer, fader: usize) -> MidiCc {
        let slot = layer_slot(layer) + fader.min(3);
        self.slots[slot.min(11)]
    }
}

fn layer_slot(layer: LatchLayer) -> usize {
    match layer {
        LatchLayer::Main => 0,
        LatchLayer::Alt => 4,
        LatchLayer::Third => 8,
    }
}

fn apply_device_defaults(p: &mut Params, device: usize) {
    let idx = device.min(DEVICE_COUNT - 1);
    let defaults = DEFAULT_CCS[idx];
    for (i, cc) in defaults.iter().enumerate() {
        p.slots[i] = MidiCc::from(*cc);
    }
}

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        // 15 = pre-Color layout (MidiOut at 14); 16 = Color + MidiOut.
        if values.len() < 15 {
            return None;
        }
        let mut slots = [MidiCc::from(1u8); 12];
        for i in 0..12 {
            slots[i] = MidiCc::from_value(values[2 + i]);
        }
        let (color, midi_out) = if values.len() >= PARAMS {
            (
                Color::from_value(values[14]),
                MidiOut::from_value(values[15]),
            )
        } else {
            (Color::Cyan, MidiOut::from_value(values[14]))
        };
        Some(Self {
            device: usize::from_value(values[0]),
            midi_channel: MidiChannel::from_value(values[1]),
            slots,
            color,
            midi_out,
        })
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        let mut vec = Vec::new();
        vec.push(self.device.into()).unwrap();
        vec.push(self.midi_channel.into()).unwrap();
        for cc in &self.slots {
            vec.push((*cc).into()).unwrap();
        }
        vec.push(self.color.into()).unwrap();
        vec.push(self.midi_out.into()).unwrap();
        vec
    }
}

#[derive(Serialize, Deserialize)]
pub struct Storage {
    main: [u16; 4],
    alt: [u16; 4],
    third: [u16; 4],
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            main: [4095, 4095, 4095, 4095],
            alt: [4095, 4095, 4095, 4095],
            third: [4095, 4095, 4095, 4095],
        }
    }
}

impl AppStorage for Storage {}

fn value_for(storage: &Storage, layer: LatchLayer, ch: usize) -> u16 {
    let ch = ch.min(3);
    match layer {
        LatchLayer::Main => storage.main[ch],
        LatchLayer::Alt => storage.alt[ch],
        LatchLayer::Third => storage.third[ch],
    }
}

/// Per-channel latch — Shift and button work in parallel:
/// button hold → Third on that channel; else Shift → Alt; else Main.
/// Shift + channel button short tap (no fader move) resets Main, Alt, and Third for that channel.
fn layer_for_channel(shift: bool, button: bool) -> LatchLayer {
    if button {
        LatchLayer::Third
    } else if shift {
        LatchLayer::Alt
    } else {
        LatchLayer::Main
    }
}

fn layer_accent(layer: LatchLayer, color: Color) -> Color {
    match layer {
        LatchLayer::Main => color,
        LatchLayer::Alt => shift_hue(color, 20),
        LatchLayer::Third => shift_hue(color, -20),
    }
}

/// Rotate hue of a palette/Custom color by `degrees` (−360…360), keep S/V.
fn shift_hue(color: Color, degrees: i16) -> Color {
    let RGB8 { r, g, b } = RGB8::from(color);
    let (h, s, v) = rgb_to_hsv(r, g, b);
    let h = wrap_degrees(h + degrees);
    if s < 13 {
        // Near-grey: hue undefined — bump sat so ±shift is visible.
        let (nr, ng, nb) = hsv_to_rgb(h, 90, v.max(51));
        return Color::Custom(nr, ng, nb);
    }
    let (nr, ng, nb) = hsv_to_rgb(h, s, v);
    Color::Custom(nr, ng, nb)
}

fn wrap_degrees(d: i16) -> u16 {
    let mut d = d % 360;
    if d < 0 {
        d += 360;
    }
    d as u16
}

/// HSV with H in 0..360, S/V in 0..255.
fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (i16, u8, u8) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let v = max;
    let d = max - min;
    let s = if max == 0 {
        0
    } else {
        ((d as u16 * 255) / max as u16) as u8
    };
    if d == 0 {
        return (0, s, v);
    }
    let h = if max == r {
        60 * (i16::from(g) - i16::from(b)) / i16::from(d)
    } else if max == g {
        120 + 60 * (i16::from(b) - i16::from(r)) / i16::from(d)
    } else {
        240 + 60 * (i16::from(r) - i16::from(g)) / i16::from(d)
    };
    (wrap_degrees(h) as i16, s, v)
}

fn hsv_to_rgb(h: u16, s: u8, v: u8) -> (u8, u8, u8) {
    if s == 0 {
        return (v, v, v);
    }
    let region = h / 60;
    let rem = (h % 60) as u32;
    let v = v as u32;
    let s = s as u32;
    let p = v * (255 - s) / 255;
    let q = v * (255 - (s * rem) / 60) / 255;
    let t = v * (255 - (s * (60 - rem)) / 60) / 255;
    let (r, g, b) = match region {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    (r as u8, g as u8, b as u8)
}

#[embassy_executor::task(pool_size = 16 / CHANNELS)]
pub async fn wrapper(app: App<CHANNELS>, exit_signal: &'static Signal<NoopRawMutex, bool>) {
    let device = 0usize;
    let mut initial = Params {
        device,
        midi_channel: MidiChannel::default(),
        slots: [MidiCc::from(1u8); 12],
        color: Color::Cyan,
        midi_out: MidiOut::default(),
    };
    apply_device_defaults(&mut initial, device);

    let param_store = ParamStore::<Params>::new(app.app_id, app.layout_id, initial);
    let storage = ManagedStorage::<Storage>::new(app.app_id, app.layout_id);

    param_store.load().await;
    storage.load().await;

    let app_loop = async {
        let mut prev_device = param_store.query(|p| p.device);
        loop {
            select3(
                run(&app, &param_store, &storage),
                param_store.param_handler(),
                storage.saver_task(),
            )
            .await;

            let cur = param_store.query(|p| p.device);
            if cur != prev_device {
                let idx = cur;
                param_store.update(|p| apply_device_defaults(p, idx)).await;
                prev_device = cur;
            }
        }
    };

    select(app_loop, app.exit_handler(exit_signal)).await;
}

pub async fn run(
    app: &App<CHANNELS>,
    params: &ParamStore<Params>,
    storage: &ManagedStorage<Storage>,
) {
    let (midi_channel, midi_out) = params.query(|p| (p.midi_channel, p.midi_out));
    let midi = app.use_midi_output(midi_out, midi_channel, false);

    let faders = app.use_faders();
    let buttons = app.use_buttons();
    let leds = app.use_leds();

    let layers_glob = app.make_global([LatchLayer::Main; CHANNELS]);
    let long_press_fired = app.make_global(false);
    let latches_glob = app.make_global([
        app.make_latch(faders.get_value_at(0)),
        app.make_latch(faders.get_value_at(1)),
        app.make_latch(faders.get_value_at(2)),
        app.make_latch(faders.get_value_at(3)),
    ]);
    let last_gate_glob = app.make_global([u32::MAX; CHANNELS]);

    let midi_task = async {
        loop {
            app.delay_millis(1).await;
            if !midi_out.is_some() {
                continue;
            }
            let layers = layers_glob.get();
            let mut last_gate = last_gate_glob.get();
            for fader in 0..CHANNELS {
                let layer = layers[fader];
                let val = storage.query(|s| value_for(s, layer, fader));
                let cc = params.query(|p| p.cc_for_layer(layer, fader));
                let gate = midi_gate(val, false) as u32;
                if gate != last_gate[fader] {
                    midi.send_cc(cc, val).await;
                    last_gate[fader] = gate;
                }
            }
            last_gate_glob.set(last_gate);
        }
    };

    let layer_task = async {
        loop {
            app.delay_millis(1).await;
            let shift = buttons.is_shift_pressed();
            let mut layers = [LatchLayer::Main; CHANNELS];
            for ch in 0..CHANNELS {
                layers[ch] = layer_for_channel(shift, buttons.is_button_pressed(ch));
            }
            layers_glob.set(layers);

            let color = params.query(|p| p.color);
            for ch in 0..CHANNELS {
                let layer = layers[ch];
                let val = storage.query(|s| value_for(s, layer, ch));
                let led = split_unsigned_value(val);
                let accent = layer_accent(layer, color);
                // Button + Top/Bottom: layer accent; button brightness = latched value.
                leds.set(
                    ch,
                    Led::Button,
                    accent,
                    Brightness::Custom((val / 16) as u8),
                );
                leds.set(ch, Led::Top, accent, Brightness::Custom(led[0]));
                leds.set(ch, Led::Bottom, accent, Brightness::Custom(led[1]));
            }
        }
    };

    let fader_task = async {
        loop {
            let chan = faders.wait_for_any_change().await;
            let layer = layers_glob.get()[chan];
            let target = storage.query(|s| value_for(s, layer, chan));
            let mut latches = latches_glob.get();
            if let Some(new_value) = latches[chan].update(faders.get_value_at(chan), layer, target)
            {
                latches_glob.set(latches);
                storage.modify_and_save(|s| match layer {
                    LatchLayer::Main => s.main[chan] = new_value,
                    LatchLayer::Alt => s.alt[chan] = new_value,
                    LatchLayer::Third => s.third[chan] = new_value,
                });
            } else {
                latches_glob.set(latches);
            }
        }
    };

    let reset_task = async {
        loop {
            let (chan, shift) = buttons.wait_for_any_down().await;
            if !shift {
                continue;
            }
            let fader_at_down = faders.get_value_at(chan);
            long_press_fired.set(false);
            buttons.wait_for_up(chan).await;
            if long_press_fired.get() {
                continue;
            }
            let fader_now = faders.get_value_at(chan);
            if fader_at_down.abs_diff(fader_now) > RESET_FADER_DEADBAND {
                continue;
            }
            storage.modify_and_save(|s| {
                s.main[chan] = 0;
                s.alt[chan] = 0;
                s.third[chan] = 0;
            });
            latches_glob.modify(|l| {
                let mut l = *l;
                l[chan] = app.make_latch(faders.get_value_at(chan));
                l
            });
            last_gate_glob.modify(|g| {
                let mut g = *g;
                g[chan] = u32::MAX;
                g
            });
            // Local CC is rate-limited (~8 ms gap in midi_distributor); burst
            // try_sends would drop Alt/Third. Space the three zeros.
            if midi_out.is_some() {
                for (i, layer) in [LatchLayer::Main, LatchLayer::Alt, LatchLayer::Third]
                    .into_iter()
                    .enumerate()
                {
                    let cc = params.query(|p| p.cc_for_layer(layer, chan));
                    midi.send_cc(cc, 0).await;
                    if i < 2 {
                        app.delay_millis(10).await;
                    }
                }
            }
        }
    };

    let long_press_task = async {
        loop {
            buttons.wait_for_any_long_press().await;
            long_press_fired.set(true);
        }
    };

    let scene_handler = async {
        loop {
            match app.wait_for_scene_event().await {
                SceneEvent::LoadScene(scene) => storage.load_from_scene(scene).await,
                SceneEvent::SaveScene(scene) => storage.save_to_scene(scene).await,
            }
        }
    };

    join(
        long_press_task,
        join5(midi_task, layer_task, fader_task, reset_task, scene_handler),
    )
    .await;
}
