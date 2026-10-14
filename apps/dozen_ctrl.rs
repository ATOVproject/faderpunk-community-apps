//! Dozen Ctrl — 4×3 latched MIDI CC banks for one synth on one channel.
//!
//! Full per-device CC maps live in `docs/apps/123-dozen-ctrl/device-cc-catalog.json`
//! (recommended ★ entries match firmware `DEFAULT_CCS` when you change Device).

#[path = "dozen_ctrl_defaults.rs"]
mod dozen_ctrl_defaults;

use embassy_futures::{
    join::join4,
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

use crate::app::{App, AppParams, AppStorage, Led, ManagedStorage, ParamStore, SceneEvent};

use dozen_ctrl_defaults::{DEFAULT_CCS, DEVICE_COUNT, DEVICE_NAMES};

pub const CHANNELS: usize = 4;
pub const PARAMS: usize = 15;

const LED_COLOR: Color = Color::Cyan;
const LED_BRIGHTNESS: Brightness = Brightness::Mid;

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
.add_param(Param::MidiOut);

pub struct Params {
    device: usize,
    midi_channel: MidiChannel,
    slots: [MidiCc; 12],
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
        if values.len() < PARAMS {
            return None;
        }
        let mut slots = [MidiCc::from(1u8); 12];
        for i in 0..12 {
            slots[i] = MidiCc::from_value(values[2 + i]);
        }
        Some(Self {
            device: usize::from_value(values[0]),
            midi_channel: MidiChannel::from_value(values[1]),
            slots,
            midi_out: MidiOut::from_value(values[14]),
        })
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        let mut vec = Vec::new();
        vec.push(self.device.into()).unwrap();
        vec.push(self.midi_channel.into()).unwrap();
        for cc in &self.slots {
            vec.push((*cc).into()).unwrap();
        }
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

fn layer_values(storage: &Storage, layer: LatchLayer) -> [u16; 4] {
    match layer {
        LatchLayer::Main => storage.main,
        LatchLayer::Alt => storage.alt,
        LatchLayer::Third => storage.third,
    }
}

#[embassy_executor::task(pool_size = 16 / CHANNELS)]
pub async fn wrapper(app: App<CHANNELS>, exit_signal: &'static Signal<NoopRawMutex, bool>) {
    let device = 0usize;
    let mut initial = Params {
        device,
        midi_channel: MidiChannel::default(),
        slots: [MidiCc::from(1u8); 12],
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
                param_store
                    .update(|p| apply_device_defaults(p, idx))
                    .await;
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

    let latch_glob = app.make_global(LatchLayer::Main);

    for ch in 0..CHANNELS {
        leds.set(ch, Led::Button, LED_COLOR, LED_BRIGHTNESS);
    }

    let midi_task = async {
        let mut last_gate = [u32::MAX; 4];
        loop {
            app.delay_millis(1).await;
            if !midi_out.is_some() {
                continue;
            }
            let layer = latch_glob.get();
            let values = storage.query(|s| layer_values(s, layer));
            for fader in 0..CHANNELS {
                let cc = params.query(|p| p.cc_for_layer(layer, fader));
                let val = values[fader];
                let gate = midi_gate(val, false) as u32;
                if gate != last_gate[fader] {
                    midi.send_cc(cc, val).await;
                    last_gate[fader] = gate;
                }
            }
        }
    };

    let layer_task = async {
        loop {
            app.delay_millis(1).await;
            let layer = if buttons.is_shift_pressed() && !buttons.is_button_pressed(0) {
                LatchLayer::Alt
            } else if !buttons.is_shift_pressed() && buttons.is_button_pressed(0) {
                LatchLayer::Third
            } else {
                LatchLayer::Main
            };
            latch_glob.set(layer);

            for ch in 0..CHANNELS {
                let vals = storage.query(|s| layer_values(s, layer));
                let led = split_unsigned_value(vals[ch]);
                let accent = match layer {
                    LatchLayer::Main => LED_COLOR,
                    LatchLayer::Alt => Color::Orange,
                    LatchLayer::Third => Color::Violet,
                };
                leds.set(ch, Led::Top, accent, Brightness::Custom(led[0]));
                leds.set(ch, Led::Bottom, accent, Brightness::Custom(led[1]));
            }
        }
    };

    let fader_task = async {
        let mut latches = [
            app.make_latch(faders.get_value_at(0)),
            app.make_latch(faders.get_value_at(1)),
            app.make_latch(faders.get_value_at(2)),
            app.make_latch(faders.get_value_at(3)),
        ];
        loop {
            let chan = faders.wait_for_any_change().await;
            let layer = latch_glob.get();
            let target = storage.query(|s| layer_values(s, layer))[chan];
            if let Some(new_value) = latches[chan].update(faders.get_value_at(chan), layer, target)
            {
                storage.modify_and_save(|s| match layer {
                    LatchLayer::Main => s.main[chan] = new_value,
                    LatchLayer::Alt => s.alt[chan] = new_value,
                    LatchLayer::Third => s.third[chan] = new_value,
                });
            }
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

    join4(midi_task, layer_task, fader_task, scene_handler).await;
}
