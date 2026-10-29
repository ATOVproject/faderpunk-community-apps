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
/// Number of MIDI destinations the fader can drive.
const SLOTS: usize = 6;
/// Per slot: channel, CC.
const PARAMS_PER_SLOT: usize = 2;
/// Slot count + the slots + color + mute level.
pub const PARAMS: usize = 1 + SLOTS * PARAMS_PER_SLOT + 2;

/// Enum index 0 is "Off", index n is MIDI channel n.
const CHANNEL_VARIANTS: &[&str] = &[
    "Off", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
];

pub static CONFIG: Config<PARAMS> = Config::new(
    "Macro Control",
    "One fader sending up to 6 MIDI CCs, button mutes them to a set level",
    Color::Violet,
    AppIcon::Fader,
)
.add_param(Param::i32 {
    name: "Number of CCs",
    min: 1,
    max: SLOTS as i32,
})
.add_param(Param::Enum {
    name: "CC 1 Channel",
    variants: CHANNEL_VARIANTS,
})
.add_param(Param::MidiCc { name: "CC 1 Number" })
.add_param(Param::Enum {
    name: "CC 2 Channel",
    variants: CHANNEL_VARIANTS,
})
.add_param(Param::MidiCc { name: "CC 2 Number" })
.add_param(Param::Enum {
    name: "CC 3 Channel",
    variants: CHANNEL_VARIANTS,
})
.add_param(Param::MidiCc { name: "CC 3 Number" })
.add_param(Param::Enum {
    name: "CC 4 Channel",
    variants: CHANNEL_VARIANTS,
})
.add_param(Param::MidiCc { name: "CC 4 Number" })
.add_param(Param::Enum {
    name: "CC 5 Channel",
    variants: CHANNEL_VARIANTS,
})
.add_param(Param::MidiCc { name: "CC 5 Number" })
.add_param(Param::Enum {
    name: "CC 6 Channel",
    variants: CHANNEL_VARIANTS,
})
.add_param(Param::MidiCc { name: "CC 6 Number" })
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
.add_param(Param::i32 {
    name: "Mute Level",
    min: 0,
    max: 127,
});

#[derive(Clone, Copy)]
struct Slot {
    /// 0 = off, 1-16 = MIDI channel.
    channel: usize,
    cc: MidiCc,
}

pub struct Params {
    count: i32,
    slots: [Slot; SLOTS],
    color: Color,
    mute_level: i32,
}

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.len() < PARAMS {
            return None;
        }
        let tail = 1 + SLOTS * PARAMS_PER_SLOT;
        Some(Self {
            count: i32::from_value(values[0]),
            slots: core::array::from_fn(|i| {
                let base = 1 + i * PARAMS_PER_SLOT;
                Slot {
                    channel: usize::from_value(values[base]),
                    cc: MidiCc::from_value(values[base + 1]),
                }
            }),
            color: Color::from_value(values[tail]),
            mute_level: i32::from_value(values[tail + 1]),
        })
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        let mut vec = Vec::new();
        // PARAMS <= APP_MAX_PARAMS is asserted by Config, so the pushes cannot fail.
        vec.push(self.count.into()).unwrap();
        for slot in &self.slots {
            vec.push(slot.channel.into()).unwrap();
            vec.push(slot.cc.into()).unwrap();
        }
        vec.push(self.color.into()).unwrap();
        vec.push(self.mute_level.into()).unwrap();
        vec
    }
}

#[derive(Serialize, Deserialize)]
pub struct Storage {
    /// Fader position, 0-4095.
    fad_val: u16,
    muted: bool,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            fad_val: 0,
            muted: false,
        }
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
            count: SLOTS as i32,
            slots: core::array::from_fn(|i| Slot {
                channel: 1,
                cc: MidiCc::from(32u8.saturating_add(ch.saturating_mul(SLOTS as u8) + i as u8)),
            }),
            color: Color::Violet,
            mute_level: 0,
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

/// Map a 0-127 CC value onto the 12-bit domain the MIDI output takes.
/// `n * 32` lands exactly on 7-bit value `n`, and 127 uses full scale.
fn to_12bit(n: i32) -> u16 {
    let n = n.clamp(0, 127);
    if n == 127 {
        4095
    } else {
        (n * 32) as u16
    }
}

pub async fn run(
    app: &App<CHANNELS>,
    params: &ParamStore<Params>,
    storage: &ManagedStorage<Storage>,
) {
    let (count, slots, color, mute_level) =
        params.query(|p| (p.count, p.slots, p.color, p.mute_level));
    let active = count.clamp(1, SLOTS as i32) as usize;
    let mute_level = to_12bit(mute_level);

    let fader = app.use_faders();
    let buttons = app.use_buttons();
    let leds = app.use_leds();
    let midi = core::array::from_fn::<_, SLOTS, _>(|i| {
        app.use_midi_output(
            MidiOut::default(),
            MidiChannel::from(slots[i].channel.clamp(0, 16) as u8),
            false,
        )
    });

    let main_loop = async {
        let mut latch = app.make_latch(fader.get_value());
        // Every active slot sends once on start so the receiver matches the fader.
        let mut last = [u32::MAX; SLOTS];

        loop {
            app.delay_millis(1).await;

            // The fader keeps tracking while muted, so unmuting resumes at its
            // current position.
            let stored = storage.query(|s| s.fad_val);
            if let Some(new_value) = latch.update(fader.get_value(), LatchLayer::Main, stored) {
                storage.modify(|s| s.fad_val = new_value);
            }
            let (value, muted) = storage.query(|s| (s.fad_val, s.muted));

            if muted {
                leds.unset(0, Led::Button);
                leds.unset(0, Led::Top);
            } else {
                leds.set(0, Led::Button, color, Brightness::Mid);
                leds.set(0, Led::Top, color, Brightness::Custom((value / 16) as u8));
            }
            leds.unset(0, Led::Bottom);

            for (i, slot) in slots.iter().enumerate().take(active) {
                if slot.channel == 0 {
                    continue;
                }
                let out = if muted { mute_level } else { value };
                let gated = midi_gate(out, false) as u32;
                if last[i] != gated {
                    midi[i].send_cc(slot.cc, out).await;
                    last[i] = gated;
                }
            }
        }
    };

    let button_handler = async {
        loop {
            buttons.wait_for_down(0).await;
            storage.modify_and_save(|s| s.muted = !s.muted);
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
