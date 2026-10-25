use embassy_futures::{
    join::join3,
    select::{select, select3},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use heapless::Vec;
use libfp::{
    ext::FromValue, latch::LatchLayer, utils::midi_gate, AppIcon, Brightness, Color, Config,
    MidiCc, MidiChannel, MidiOut, Param, Value, APP_MAX_PARAMS,
};
use serde::{Deserialize, Serialize};

use crate::app::{App, AppParams, AppStorage, Led, ManagedStorage, ParamStore, SceneEvent};

pub const CHANNELS: usize = 1;
/// Number of MIDI channels (slots) the fader and button can drive.
const SLOTS: usize = 3;
/// Per slot: channel, fader CC, fader min, fader max, button CC.
const PARAMS_PER_SLOT: usize = 5;
/// The slots + NRPN.
pub const PARAMS: usize = SLOTS * PARAMS_PER_SLOT + 1;

/// Enum index 0 is "Off", index n is MIDI channel n.
const CHANNEL_VARIANTS: &[&str] = &[
    "Off", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
];

pub static CONFIG: Config<PARAMS> = Config::new(
    "Macro Control",
    "One fader and button sending scaled MIDI CCs on up to 3 channels",
    Color::Violet,
    AppIcon::Fader,
)
.add_param(Param::Enum {
    name: "Slot 1 Channel",
    variants: CHANNEL_VARIANTS,
})
.add_param(Param::MidiCc {
    name: "Slot 1 Fader CC",
})
.add_param(Param::i32 {
    name: "Slot 1 Fader Min",
    min: 0,
    max: 127,
})
.add_param(Param::i32 {
    name: "Slot 1 Fader Max",
    min: 0,
    max: 127,
})
.add_param(Param::MidiCc {
    name: "Slot 1 Button CC",
})
.add_param(Param::Enum {
    name: "Slot 2 Channel",
    variants: CHANNEL_VARIANTS,
})
.add_param(Param::MidiCc {
    name: "Slot 2 Fader CC",
})
.add_param(Param::i32 {
    name: "Slot 2 Fader Min",
    min: 0,
    max: 127,
})
.add_param(Param::i32 {
    name: "Slot 2 Fader Max",
    min: 0,
    max: 127,
})
.add_param(Param::MidiCc {
    name: "Slot 2 Button CC",
})
.add_param(Param::Enum {
    name: "Slot 3 Channel",
    variants: CHANNEL_VARIANTS,
})
.add_param(Param::MidiCc {
    name: "Slot 3 Fader CC",
})
.add_param(Param::i32 {
    name: "Slot 3 Fader Min",
    min: 0,
    max: 127,
})
.add_param(Param::i32 {
    name: "Slot 3 Fader Max",
    min: 0,
    max: 127,
})
.add_param(Param::MidiCc {
    name: "Slot 3 Button CC",
})
.add_param(Param::MidiNrpn);

#[derive(Clone, Copy)]
struct Slot {
    /// 0 = off, 1-16 = MIDI channel.
    channel: usize,
    fader_cc: MidiCc,
    min: i32,
    max: i32,
    button_cc: MidiCc,
}

pub struct Params {
    slots: [Slot; SLOTS],
    nrpn: bool,
}

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.len() < PARAMS {
            return None;
        }
        Some(Self {
            slots: core::array::from_fn(|i| {
                let base = i * PARAMS_PER_SLOT;
                Slot {
                    channel: usize::from_value(values[base]),
                    fader_cc: MidiCc::from_value(values[base + 1]),
                    min: i32::from_value(values[base + 2]),
                    max: i32::from_value(values[base + 3]),
                    button_cc: MidiCc::from_value(values[base + 4]),
                }
            }),
            nrpn: bool::from_value(values[SLOTS * PARAMS_PER_SLOT]),
        })
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        let mut vec = Vec::new();
        // PARAMS <= APP_MAX_PARAMS is asserted by Config, so the pushes cannot fail.
        for slot in &self.slots {
            vec.push(slot.channel.into()).unwrap();
            vec.push(slot.fader_cc.into()).unwrap();
            vec.push(slot.min.into()).unwrap();
            vec.push(slot.max.into()).unwrap();
            vec.push(slot.button_cc.into()).unwrap();
        }
        vec.push(Value::MidiNrpn(self.nrpn)).unwrap();
        vec
    }
}

#[derive(Serialize, Deserialize)]
pub struct Storage {
    /// Fader position, 0-4095.
    fad_val: u16,
}

impl Default for Storage {
    fn default() -> Self {
        Self { fad_val: 0 }
    }
}

impl AppStorage for Storage {}

#[embassy_executor::task(pool_size = 16/CHANNELS)]
pub async fn wrapper(app: App<CHANNELS>, exit_signal: &'static Signal<NoopRawMutex, bool>) {
    let ch = app.start_channel as u8;
    let param_store = ParamStore::<Params>::new(
        app.app_id,
        app.layout_id,
        Params {
            slots: core::array::from_fn(|i| {
                let offset = ch.saturating_mul(SLOTS as u8) + i as u8;
                Slot {
                    channel: 1,
                    fader_cc: MidiCc::from(32u8.saturating_add(offset)),
                    min: 0,
                    max: 127,
                    button_cc: MidiCc::from(64u8.saturating_add(offset)),
                }
            }),
            nrpn: false,
        },
    );
    let storage = ManagedStorage::<Storage>::new(app.app_id, app.layout_id);

    param_store.load().await;
    storage.load().await;

    let app_loop = async {
        loop {
            select3(
                run(&app, &param_store, &storage),
                param_store.param_handler(),
                storage.saver_task(),
            )
            .await;
        }
    };

    select(app_loop, app.exit_handler(exit_signal)).await;
}

/// Map a 0-127 CC bound onto the 12-bit domain the MIDI output takes.
/// `n * 32` lands exactly on 7-bit value `n`; 127 uses full scale so NRPN
/// reaches its top value.
fn bound_12bit(n: i32) -> i32 {
    let n = n.clamp(0, 127);
    if n == 127 {
        4095
    } else {
        n * 32
    }
}

/// Scale the fader (0-4095) between `min` and `max`. When `min > max` the
/// span is negative, so the response is reversed.
fn scale_fader(fader: u16, min: i32, max: i32) -> u16 {
    let lo = bound_12bit(min);
    let hi = bound_12bit(max);
    (lo + ((hi - lo) * fader as i32) / 4095).clamp(0, 4095) as u16
}

pub async fn run(
    app: &App<CHANNELS>,
    params: &ParamStore<Params>,
    storage: &ManagedStorage<Storage>,
) {
    let (slots, nrpn) = params.query(|p| (p.slots, p.nrpn));

    let fader = app.use_faders();
    let buttons = app.use_buttons();
    let leds = app.use_leds();
    let midi = core::array::from_fn::<_, SLOTS, _>(|i| {
        app.use_midi_output(
            MidiOut::default(),
            MidiChannel::from(slots[i].channel.clamp(0, 16) as u8),
            nrpn,
        )
    });

    leds.set(0, Led::Button, Color::Violet, Brightness::Mid);

    let main_loop = async {
        let mut latch = app.make_latch(fader.get_value());
        // Every active slot sends once on start so the receiver matches the fader.
        let mut last = [u32::MAX; SLOTS];

        loop {
            app.delay_millis(1).await;

            let stored = storage.query(|s| s.fad_val);
            if let Some(new_value) = latch.update(fader.get_value(), LatchLayer::Main, stored) {
                storage.modify(|s| s.fad_val = new_value);
            }
            let value = storage.query(|s| s.fad_val);

            leds.set(
                0,
                Led::Top,
                Color::Violet,
                Brightness::Custom((value / 16) as u8),
            );
            leds.unset(0, Led::Bottom);

            for (i, slot) in slots.iter().enumerate() {
                if slot.channel == 0 {
                    continue;
                }
                let out = scale_fader(value, slot.min, slot.max);
                let gated = midi_gate(out, nrpn) as u32;
                if last[i] != gated {
                    midi[i].send_cc(slot.fader_cc, out).await;
                    last[i] = gated;
                }
            }
        }
    };

    // Momentary: full scale on every active slot's button CC while held, 0 on release.
    let button_handler = async {
        loop {
            buttons.wait_for_down(0).await;
            leds.set(0, Led::Button, Color::Violet, Brightness::High);
            for (i, slot) in slots.iter().enumerate() {
                if slot.channel != 0 {
                    midi[i].send_cc(slot.button_cc, 4095).await;
                }
            }

            buttons.wait_for_up(0).await;
            leds.set(0, Led::Button, Color::Violet, Brightness::Mid);
            for (i, slot) in slots.iter().enumerate() {
                if slot.channel != 0 {
                    midi[i].send_cc(slot.button_cc, 0).await;
                }
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

    join3(main_loop, button_handler, scene_handler).await;
}
