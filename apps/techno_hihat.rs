use embassy_futures::{
    join::{join, join5},
    select::{select, select3},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use heapless::Vec;
use serde::{Deserialize, Serialize};

use libfp::{
    ext::FromValue, latch::LatchLayer, utils::euclidean_at, AppIcon, Brightness, ClockDivision,
    Color, Config, MidiChannel, MidiNote, MidiOut, Param, Value, APP_MAX_PARAMS,
};

use crate::app::{
    App, AppParams, AppStorage, ClockEvent, Led, ManagedStorage, ParamStore, SceneEvent,
};

pub const CHANNELS: usize = 1;
pub const PARAMS: usize = 6;

const LED_BRIGHTNESS: Brightness = Brightness::Mid;

// At 24 PPQN, one 16th-note step = 6 clock ticks
const STEP_TICKS: u32 = 6;
const PATTERN_LEN: usize = 16;
const FAMILIES: usize = 4;
const VARIATIONS: usize = 4;
const FADER_MOVE_THRESH: u16 = 32;

// Jack Mode enum indices
const JACK_OUT_ALL: usize = 0;
const JACK_OUT_ACCENTS: usize = 1;
const JACK_OUT_GHOSTS: usize = 2;

// 12-bit MIDI velocity levels (libfp scales 0..=4095 to MIDI 0..=127)
const ACCENT_VELOCITY: u16 = 4095; // ~127 MIDI velocity
const BASE_VELOCITY: u16 = 2100;   // ~65 MIDI velocity for unaccented ghost/groove hits

// 4 Families (Main Fader) x 4 Variations (Fn + Fader)
const PATTERNS: [[[u8; PATTERN_LEN]; VARIATIONS]; FAMILIES] = [
    // Family 0 (Bottom): Off-Beat Archetypes
    [
        // Var 0: Pure Off-Beat (..x. ..x. ..x. ..x.)
        [0, 0, 1, 0,  0, 0, 1, 0,  0, 0, 1, 0,  0, 0, 1, 0],
        // Var 1: Off-Beat + Bar-End 16th Pickup (..x. ..x. ..x. ..xx)
        [0, 0, 1, 0,  0, 0, 1, 0,  0, 0, 1, 0,  0, 0, 1, 1],
        // Var 2: Half-Bar Gallop (..x. ..xx ..x. ..xx)
        [0, 0, 1, 0,  0, 0, 1, 1,  0, 0, 1, 0,  0, 0, 1, 1],
        // Var 3: Full 909 Gallop (..xx ..xx ..xx ..xx)
        [0, 0, 1, 1,  0, 0, 1, 1,  0, 0, 1, 1,  0, 0, 1, 1],
    ],
    // Family 1 (Lower-Mid): 8th-Note Archetypes
    [
        // Var 0: Straight 8ths (x.x. x.x. x.x. x.x.)
        [1, 0, 1, 0,  1, 0, 1, 0,  1, 0, 1, 0,  1, 0, 1, 0],
        // Var 1: 8ths + Bar-End Pickup (x.x. x.x. x.x. x.xx)
        [1, 0, 1, 0,  1, 0, 1, 0,  1, 0, 1, 0,  1, 0, 1, 1],
        // Var 2: Alternating Chug (x.x. x.xx x.x. x.xx)
        [1, 0, 1, 0,  1, 0, 1, 1,  1, 0, 1, 0,  1, 0, 1, 1],
        // Var 3: Driving Chug (x.xx x.xx x.xx x.xx)
        [1, 0, 1, 1,  1, 0, 1, 1,  1, 0, 1, 1,  1, 0, 1, 1],
    ],
    // Family 2 (Upper-Mid): 16th-Note Archetypes
    [
        // Var 0: Rolling 16ths (xxxx xxxx xxxx xxxx)
        [1, 1, 1, 1,  1, 1, 1, 1,  1, 1, 1, 1,  1, 1, 1, 1],
        // Var 1: Breathing / Ducked 16ths - drops downbeats (.xxx .xxx .xxx .xxx)
        [0, 1, 1, 1,  0, 1, 1, 1,  0, 1, 1, 1,  0, 1, 1, 1],
        // Var 2: 3-16th Stutter Roll (xxx. xxx. xxx. xxxx)
        [1, 1, 1, 0,  1, 1, 1, 0,  1, 1, 1, 0,  1, 1, 1, 1],
        // Var 3: Syncopated Off-Grid 16ths (.xx. .xx. .xx. .xxx)
        [0, 1, 1, 0,  0, 1, 1, 0,  0, 1, 1, 0,  0, 1, 1, 1],
    ],
    // Family 3 (Top): On-Beat / Industrial Archetypes
    [
        // Var 0: Straight 4x4 On-Beat (x... x... x... x...)
        [1, 0, 0, 0,  1, 0, 0, 0,  1, 0, 0, 0,  1, 0, 0, 0],
        // Var 1: Tresillo 3-3-2 (x..x ..x. x..x ..x.)
        [1, 0, 0, 1,  0, 0, 1, 0,  1, 0, 0, 1,  0, 0, 1, 0],
        // Var 2: Industrial Syncopated Clank (x..x x... x..x x.x.)
        [1, 0, 0, 1,  1, 0, 0, 0,  1, 0, 0, 1,  1, 0, 1, 0],
        // Var 3: Double-Tap Stomp (xx.. xx.. xx.. xx..)
        [1, 1, 0, 0,  1, 1, 0, 0,  1, 1, 0, 0,  1, 1, 0, 0],
    ],
];

// Bottom LED indicator color for each main fader archetype
const FAMILY_COLORS: [Color; FAMILIES] = [
    Color::Cyan,   // 0: Off-beat family
    Color::Blue,   // 1: 8th-note family
    Color::Orange, // 2: 16th-note family
    Color::Pink,   // 3: On-beat / Industrial family
];

#[inline]
fn value_to_family(val: u16) -> usize {
    ((val as usize) * FAMILIES / 4096).min(FAMILIES - 1)
}

#[inline]
fn value_to_variation(val: u16) -> usize {
    ((val as usize) * VARIATIONS / 4096).min(VARIATIONS - 1)
}

#[inline]
fn value_to_accent_pulses(val: u16) -> u8 {
    // Maps 0..=4095 into 0..=16 Euclidean accent pulses (0 = no accents, 16 = all accented)
    ((val as u32 * 17) / 4096).min(16) as u8
}

#[inline]
fn hit_index_at(fam_idx: usize, var_idx: usize, global_step: u32) -> u32 {
    let pat = &PATTERNS[fam_idx][var_idx];
    let bar = global_step / (PATTERN_LEN as u32);
    let step_in_bar = (global_step as usize) % PATTERN_LEN;

    let mut hits_per_bar = 0_u32;
    let mut hits_before = 0_u32;
    let mut i = 0;
    while i < PATTERN_LEN {
        if pat[i] > 0 {
            if i < step_in_bar {
                hits_before += 1;
            }
            hits_per_bar += 1;
        }
        i += 1;
    }

    bar * hits_per_bar + hits_before
}

pub static CONFIG: Config<PARAMS> = Config::new(
    "Techno Hi-Hat",
    "Clocked hi-hat matrix with variations and accents",
    Color::Blue,
    AppIcon::Euclid,
)
.add_param(Param::MidiChannel {
    name: "MIDI Channel",
})
.add_param(Param::MidiNote {
    name: "MIDI Note",
})
.add_param(Param::i32 {
    name: "GATE %",
    min: 1,
    max: 100,
})
.add_param(Param::Enum {
    name: "Jack",
    variants: &[
        "Gate Out: All Hits",
        "Gate Out: Accents Only",
        "Gate Out: Ghosts Only",
    ],
})
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
    midi_channel: MidiChannel,
    note: MidiNote,
    gatel: i32,
    jack_mode: usize,
    color: Color,
    midi_out: MidiOut,
}

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.len() < PARAMS {
            return None;
        }
        let (color_idx, midi_out_idx) = if values.len() >= 7 { (5, 6) } else { (4, 5) };

        Some(Self {
            midi_channel: MidiChannel::from_value(values[0]),
            note: MidiNote::from_value(values[1]),
            gatel: i32::from_value(values[2]),
            jack_mode: usize::from_value(values[3]).min(JACK_OUT_GHOSTS),
            color: Color::from_value(values[color_idx]),
            midi_out: MidiOut::from_value(values[midi_out_idx]),
        })
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        let mut vec = Vec::new();
        vec.push(self.midi_channel.into()).unwrap(); // PARAMS (6) <= APP_MAX_PARAMS
        vec.push(self.note.into()).unwrap(); // PARAMS (6) <= APP_MAX_PARAMS
        vec.push(self.gatel.into()).unwrap(); // PARAMS (6) <= APP_MAX_PARAMS
        vec.push(self.jack_mode.into()).unwrap(); // PARAMS (6) <= APP_MAX_PARAMS
        vec.push(self.color.into()).unwrap(); // PARAMS (6) <= APP_MAX_PARAMS
        vec.push(self.midi_out.into()).unwrap(); // PARAMS (6) <= APP_MAX_PARAMS
        vec
    }
}

#[derive(Serialize, Deserialize)]
pub struct Storage {
    fader_saved: u16,
    shift_fader_saved: u16,
    fn_fader_saved: u16,
    muted: bool,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            fader_saved: 0,
            shift_fader_saved: 4095, // Default at top = all hits accented
            fn_fader_saved: 0,       // Default at bottom = Variation 0 (Pure/Classic)
            muted: false,
        }
    }
}
impl AppStorage for Storage {}

#[embassy_executor::task(pool_size = 16/CHANNELS)]
pub async fn wrapper(app: App<CHANNELS>, exit_signal: &'static Signal<NoopRawMutex, bool>) {
    let param_store = ParamStore::<Params>::new(
        app.app_id,
        app.layout_id,
        Params {
            midi_channel: MidiChannel::default(),
            note: MidiNote::from(42), // GM Closed Hi-Hat (F#1)
            gatel: 50,
            jack_mode: JACK_OUT_ALL,
            color: Color::Yellow,
            midi_out: MidiOut([false, false, false]),
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

pub async fn run(
    app: &App<CHANNELS>,
    params: &ParamStore<Params>,
    storage: &ManagedStorage<Storage>,
) {
    let (midi_out, midi_chan, note, gatel, jack_mode, led_color) = params.query(|p| {
        (
            p.midi_out,
            p.midi_channel,
            p.note,
            p.gatel as u32,
            p.jack_mode.min(JACK_OUT_GHOSTS),
            p.color,
        )
    });

    let mut clock = app.use_clock();
    let fader = app.use_faders();
    let buttons = app.use_buttons();
    let leds = app.use_leds();

    let midi = app.use_midi_output(midi_out, midi_chan, false);
    let out_jack = app.make_gate_jack(0, 4095).await;

    let glob_muted = app.make_global(false);
    let fam_glob = app.make_global(0_usize);
    let var_glob = app.make_global(0_usize);
    let accent_glob = app.make_global(16_u8);
    let glob_latch_layer = app.make_global(LatchLayer::Main);
    let third_layer_used = app.make_global(false);
    let fader_at_down = app.make_global(0_u16);
    let long_press_fired = app.make_global(false);

    let (saved_fader, saved_shift, saved_fn, mute) = storage.query(|s| {
        (
            s.fader_saved,
            s.shift_fader_saved,
            s.fn_fader_saved,
            s.muted,
        )
    });
    let initial_fam = value_to_family(saved_fader);
    let initial_var = value_to_variation(saved_fn);
    let initial_accents = value_to_accent_pulses(saved_shift);

    glob_muted.set(mute);
    fam_glob.set(initial_fam);
    var_glob.set(initial_var);
    accent_glob.set(initial_accents);

    leds.set(0, Led::Bottom, FAMILY_COLORS[initial_fam], Brightness::Low);
    if mute {
        leds.unset(0, Led::Button);
        leds.unset(0, Led::Top);
    } else {
        leds.set(0, Led::Button, led_color, LED_BRIGHTNESS);
    }

    // 1. Clock & Trigger/MIDI Execution Task
    let fut_clock = async {
        let mut note_on = false;
        let mut gate_on = false;
        let gate_step = (STEP_TICKS * gatel / 100).clamp(1, STEP_TICKS - 1);

        loop {
            match clock.wait_for_event(ClockDivision::_1).await {
                ClockEvent::Reset | ClockEvent::Stop => {
                    if note_on {
                        midi.send_note_off(note).await;
                        note_on = false;
                    }
                    if gate_on {
                        out_jack.set_low().await;
                        gate_on = false;
                    }
                    if glob_latch_layer.get() == LatchLayer::Main {
                        leds.set(0, Led::Top, led_color, Brightness::Off);
                    }
                }
                ClockEvent::Tick(tick) => {
                    let clkn = tick as u32;
                    let muted = glob_muted.get();
                    let fam_idx = fam_glob.get();
                    let var_idx = var_glob.get();
                    let pulses = accent_glob.get();

                    // Start of a 16th-note step (phase-locked to global tick)
                    if clkn.is_multiple_of(STEP_TICKS) {
                        let global_step = clkn / STEP_TICKS;
                        let step = (global_step as usize) % PATTERN_LEN;

                        if !muted && PATTERNS[fam_idx][var_idx][step] > 0 {
                            let hit_idx = hit_index_at(fam_idx, var_idx, global_step);

                            let is_accented = match pulses {
                                0 => false,
                                16.. => true,
                                k => euclidean_at(16, k, 0, hit_idx),
                            };

                            let velocity = if is_accented {
                                ACCENT_VELOCITY
                            } else {
                                BASE_VELOCITY
                            };

                            midi.send_note_on(note, velocity).await;
                            note_on = true;

                            let fire_jack = match jack_mode {
                                JACK_OUT_ALL => true,
                                JACK_OUT_ACCENTS => is_accented,
                                JACK_OUT_GHOSTS => !is_accented,
                                _ => false,
                            };

                            if fire_jack {
                                out_jack.set_high().await;
                                gate_on = true;
                            }

                            if glob_latch_layer.get() == LatchLayer::Main {
                                let hit_brightness = if is_accented {
                                    Brightness::High
                                } else {
                                    Brightness::Low
                                };
                                leds.set(0, Led::Top, led_color, hit_brightness);
                            }
                        }
                    }

                    // Gate-off point within the 16th-note step
                    if clkn % STEP_TICKS == gate_step {
                        if note_on {
                            midi.send_note_off(note).await;
                            note_on = false;
                        }
                        if gate_on {
                            out_jack.set_low().await;
                            gate_on = false;
                        }
                        if glob_latch_layer.get() == LatchLayer::Main {
                            leds.set(0, Led::Top, led_color, Brightness::Off);
                        }
                    }

                    // Show Red (Accents) or Violet (Variation) on Top LED when holding modifiers
                    match glob_latch_layer.get() {
                        LatchLayer::Alt => {
                            let shift_val = storage.query(|s| s.shift_fader_saved);
                            leds.set(
                                0,
                                Led::Top,
                                Color::Red,
                                Brightness::Custom((shift_val / 16) as u8),
                            );
                        }
                        LatchLayer::Third => {
                            let fn_val = storage.query(|s| s.fn_fader_saved);
                            leds.set(
                                0,
                                Led::Top,
                                Color::Violet,
                                Brightness::Custom((fn_val / 16) as u8),
                            );
                        }
                        LatchLayer::Main => {}
                    }
                }
                _ => {}
            }
        }
    };

    // 2. Button Task: Mute ONLY on a clean short tap release (never on down-press or Fn+Fader)
    let fut_button = async {
        loop {
            let (chan, shift) = buttons.wait_for_any_down().await;
            if !shift && chan == 0 {
                long_press_fired.set(false);
                third_layer_used.set(false);
                fader_at_down.set(fader.get_value());

                buttons.wait_for_up(0).await;

                if !third_layer_used.get() && !long_press_fired.get() {
                    let muted = glob_muted.toggle();
                    storage.modify_and_save(|s| {
                        s.muted = muted;
                    });

                    if muted {
                        out_jack.set_low().await;
                        leds.unset(0, Led::Button);
                        leds.unset(0, Led::Top);
                    } else {
                        leds.set(0, Led::Button, led_color, LED_BRIGHTNESS);
                    }
                }
            }
        }
    };

    let long_press = async {
        loop {
            buttons.wait_for_any_long_press().await;
            long_press_fired.set(true);
        }
    };

    // 3. Fader Task (Checks hardware button state directly to prevent race conditions)
    let fut_fader = async {
        let mut latch = app.make_latch(fader.get_value());
        loop {
            fader.wait_for_change_at(0).await;

            let latch_layer = if buttons.is_shift_pressed() && !buttons.is_button_pressed(0) {
                LatchLayer::Alt
            } else if !buttons.is_shift_pressed() && buttons.is_button_pressed(0) {
                third_layer_used.set(true);
                LatchLayer::Third
            } else {
                LatchLayer::Main
            };
            glob_latch_layer.set(latch_layer);

            let target_value = match latch_layer {
                LatchLayer::Main => storage.query(|s| s.fader_saved),
                LatchLayer::Alt => storage.query(|s| s.shift_fader_saved),
                LatchLayer::Third => storage.query(|s| s.fn_fader_saved),
            };

            if let Some(new_value) = latch.update(fader.get_value(), latch_layer, target_value) {
                match latch_layer {
                    LatchLayer::Main => {
                        let fam_idx = value_to_family(new_value);
                        fam_glob.set(fam_idx);
                        storage.modify_and_save(|s| s.fader_saved = new_value);
                        leds.set(0, Led::Bottom, FAMILY_COLORS[fam_idx], Brightness::Low);
                    }
                    LatchLayer::Alt => {
                        let pulses = value_to_accent_pulses(new_value);
                        accent_glob.set(pulses);
                        storage.modify_and_save(|s| s.shift_fader_saved = new_value);
                    }
                    LatchLayer::Third => {
                        let var_idx = value_to_variation(new_value);
                        var_glob.set(var_idx);
                        storage.modify_and_save(|s| s.fn_fader_saved = new_value);
                    }
                }
            }
        }
    };

    // 4. Scene Save/Recall Task
    let scene_handler = async {
        loop {
            match app.wait_for_scene_event().await {
                SceneEvent::LoadScene(scene) => {
                    storage.load_from_scene(scene).await;
                    let (saved_fader, saved_shift, saved_fn, mute) = storage.query(|s| {
                        (
                            s.fader_saved,
                            s.shift_fader_saved,
                            s.fn_fader_saved,
                            s.muted,
                        )
                    });
                    let fam_idx = value_to_family(saved_fader);
                    let var_idx = value_to_variation(saved_fn);
                    let pulses = value_to_accent_pulses(saved_shift);

                    glob_muted.set(mute);
                    fam_glob.set(fam_idx);
                    var_glob.set(var_idx);
                    accent_glob.set(pulses);

                    leds.set(0, Led::Bottom, FAMILY_COLORS[fam_idx], Brightness::Low);

                    if mute {
                        leds.unset(0, Led::Button);
                        leds.unset(0, Led::Top);
                        out_jack.set_low().await;
                    } else {
                        leds.set(0, Led::Button, led_color, LED_BRIGHTNESS);
                    }
                }
                SceneEvent::SaveScene(scene) => {
                    storage.save_to_scene(scene).await;
                }
            }
        }
    };

    // 5. Shift & Fn Layer Tracking Task
    let shift_handler = async {
        let mut prev_layer = LatchLayer::Main;
        loop {
            app.delay_millis(1).await;

            if buttons.is_button_pressed(0) && !buttons.is_shift_pressed() {
                if fader.get_value().abs_diff(fader_at_down.get()) > FADER_MOVE_THRESH {
                    third_layer_used.set(true);
                }
            }

            let active_layer = if buttons.is_shift_pressed() && !buttons.is_button_pressed(0) {
                LatchLayer::Alt
            } else if !buttons.is_shift_pressed() && buttons.is_button_pressed(0) {
                LatchLayer::Third
            } else {
                LatchLayer::Main
            };

            if active_layer != prev_layer {
                if active_layer == LatchLayer::Main {
                    leds.set(0, Led::Top, led_color, Brightness::Off);
                }
                prev_layer = active_layer;
            }
            glob_latch_layer.set(active_layer);
        }
    };

    join(
        long_press,
        join5(fut_clock, fut_button, fut_fader, scene_handler, shift_handler),
    )
    .await;
}