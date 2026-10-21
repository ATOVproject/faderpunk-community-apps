use embassy_futures::{
    join::{join, join5},
    select::{select, select3},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use heapless::Vec;
use serde::{Deserialize, Serialize};

use libfp::{
    ext::FromValue, latch::LatchLayer, AppIcon, Brightness, ClockDivision, Color, Config,
    MidiChannel, MidiNote, MidiOut, Param, Value, APP_MAX_PARAMS,
};

use crate::app::{
    App, AppParams, AppStorage, ClockEvent, Led, ManagedStorage, ParamStore, SceneEvent,
};

pub const CHANNELS: usize = 1;
pub const PARAMS: usize = 5;

const LED_BRIGHTNESS: Brightness = Brightness::Mid;

// At 24 PPQN, one 16th-note step = 6 clock ticks, one 16-step bar = 96 ticks
const STEP_TICKS: u32 = 6;
const PATTERN_LEN: usize = 16;
const BAR_TICKS: u32 = STEP_TICKS * (PATTERN_LEN as u32);
const NO_PENDING: usize = 255;
const FADER_MOVE_THRESH: u16 = 32;

// Jack Mode enum indices
const JACK_OUT_ALL: usize = 0;
const JACK_OUT_BASE: usize = 1;
const JACK_OUT_FILLS: usize = 2;

// 12-bit MIDI velocity levels
const MAIN_VELOCITY: u16 = 4095; // 100% velocity (MIDI 127) for main kicks
const FILL_VELOCITY: u16 = 3300; // ~80% velocity (MIDI 103) for turnaround fill kicks

// Step encoding:
// 0 = Rest
// 1 = Main Pattern Kick (always fires)
// 2 = Fill Stage 1 (fires on turnaround bar when Fill Density is Stage 1..=4)
// 3 = Fill Stage 2 (fires on turnaround bar when Fill Density is Stage 2..=4)
// 4 = Fill Stage 3 (fires on turnaround bar when Fill Density is Stage 3..=4)
// 5 = Fill Stage 4 (fires on turnaround bar when Fill Density is Stage 4)
const PATTERNS: &[[u8; PATTERN_LEN]] = &[
    // 1. Standard 4x4 (Bottom Hard-Stop)
    [1, 0, 0, 5,  1, 0, 0, 4,  1, 0, 0, 4,  1, 5, 3, 2],
    // 2. Karenn Lurch (Blawan-style broken stomp: 1, 2&, 3, 4&)
    [1, 0, 0, 5,  0, 0, 1, 4,  1, 0, 0, 4,  5, 3, 1, 2],
    // 3. Warehouse Heartbeat (1..a 2... 3..a 4...)
    [1, 0, 0, 1,  1, 0, 0, 4,  1, 0, 0, 1,  1, 5, 3, 2],
    // 4. Birmingham Tresillo (3-3-2 Syncopated Industrial)
    [1, 0, 0, 1,  0, 0, 1, 4,  1, 0, 0, 1,  0, 5, 1, 2],
    // 5. Broken Tail (1... 2... 3..a .&a)
    [1, 0, 0, 5,  1, 0, 0, 4,  1, 0, 0, 1,  3, 1, 5, 1],
    // 6. Asymmetric Minimal (Sparse: 1, 2&, 4 - Top Hard-Stop)
    [1, 0, 0, 0,  0, 0, 1, 4,  0, 0, 3, 0,  1, 5, 3, 2],
];

const PATTERN_COLORS: [Color; 6] = [
    Color::Cyan,   // 1: Standard 4x4
    Color::Blue,   // 2: Karenn Lurch
    Color::Green,  // 3: Warehouse Heartbeat
    Color::Yellow, // 4: Birmingham Tresillo
    Color::Orange, // 5: Broken Tail
    Color::Pink,   // 6: Asymmetric Minimal
];

#[inline]
fn value_to_pattern(val: u16) -> usize {
    let count = PATTERNS.len();
    ((val as usize) * count / 4096).min(count - 1)
}

// Maps Shift+Fader (0..=4095) to turnaround interval in bars:
// 0 = Off, 8 = every 8 bars, 4 = every 4 bars, 2 = every 2 bars, 1 = every bar
#[inline]
fn value_to_turnaround_bars(val: u16) -> u32 {
    match (val as usize * 5 / 4096).min(4) {
        0 => 0,
        1 => 8,
        2 => 4,
        3 => 2,
        _ => 1,
    }
}

// Maps Fn+Fader (0..=4095) to max active step code on turnaround bars:
// 0 = Off (no fills), 2 = Stage 1, 3 = Stage 2, 4 = Stage 3, 5 = Stage 4
#[inline]
fn value_to_fill_stage(val: u16) -> u8 {
    match (val as usize * 5 / 4096).min(4) {
        0 => 0,
        stage => (stage as u8) + 1,
    }
}

pub static CONFIG: Config<PARAMS> = Config::new(
    "Techno Kick",
    "Techno kick pattern selector with phrase fills",
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
        "Gate Out: Base Only",
        "Gate Out: Fills Only",
    ],
})
.add_param(Param::MidiOut);

pub struct Params {
    midi_channel: MidiChannel,
    note: MidiNote,
    gatel: i32,
    jack_mode: usize,
    midi_out: MidiOut,
}

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.len() < PARAMS {
            return None;
        }
        let midi_out_idx = values.len() - 1;

        Some(Self {
            midi_channel: MidiChannel::from_value(values[0]),
            note: MidiNote::from_value(values[1]),
            gatel: i32::from_value(values[2]),
            jack_mode: usize::from_value(values[3]).min(JACK_OUT_FILLS),
            midi_out: MidiOut::from_value(values[midi_out_idx]),
        })
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        let mut vec = Vec::new();
        vec.push(self.midi_channel.into()).unwrap(); // PARAMS (5) <= APP_MAX_PARAMS
        vec.push(self.note.into()).unwrap(); // PARAMS (5) <= APP_MAX_PARAMS
        vec.push(self.gatel.into()).unwrap(); // PARAMS (5) <= APP_MAX_PARAMS
        vec.push(self.jack_mode.into()).unwrap(); // PARAMS (5) <= APP_MAX_PARAMS
        vec.push(self.midi_out.into()).unwrap(); // PARAMS (5) <= APP_MAX_PARAMS
        vec
    }
}

#[derive(Serialize, Deserialize)]
pub struct Storage {
    active_pat: u8,
    fader_saved: u16,
    shift_fader_saved: u16,
    fn_fader_saved: u16,
    muted: bool,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            active_pat: 0,
            fader_saved: 0,
            shift_fader_saved: 2048, // Default: Turnaround fill every 4 bars
            fn_fader_saved: 2048,    // Default: Stage 2 fill density (4&, 4a roll)
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
            note: MidiNote::from(36), // GM Bass Drum 1 (C1)
            gatel: 50,
            jack_mode: JACK_OUT_ALL,
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
    let (midi_out, midi_chan, note, gatel, jack_mode) = params.query(|p| {
        (
            p.midi_out,
            p.midi_channel,
            p.note,
            p.gatel as u32,
            p.jack_mode.min(JACK_OUT_FILLS),
        )
    });

    let mut clock = app.use_clock();
    let fader = app.use_faders();
    let buttons = app.use_buttons();
    let leds = app.use_leds();

    let midi = app.use_midi_output(midi_out, midi_chan, false);
    let out_jack = app.make_gate_jack(0, 4095).await;

    let glob_muted = app.make_global(false);
    let active_pat_glob = app.make_global(0_usize);
    let cued_pat_glob = app.make_global(0_usize);
    let pending_pat_glob = app.make_global(NO_PENDING);
    let clock_running_glob = app.make_global(false);
    let last_tick_glob = app.make_global(0_u32);
    let turnaround_glob = app.make_global(4_u32);
    let fill_stage_glob = app.make_global(3_u8);
    let glob_latch_layer = app.make_global(LatchLayer::Main);
    let third_layer_used = app.make_global(false);
    let alt_layer_used = app.make_global(false);
    let fader_at_down = app.make_global(0_u16);
    let long_press_fired = app.make_global(false);

    let (saved_active, saved_fader, saved_shift, saved_fn, mute) = storage.query(|s| {
        (
            s.active_pat as usize,
            s.fader_saved,
            s.shift_fader_saved,
            s.fn_fader_saved,
            s.muted,
        )
    });

    let init_active = saved_active.min(PATTERNS.len() - 1);
    let init_cued = value_to_pattern(saved_fader);

    glob_muted.set(mute);
    active_pat_glob.set(init_active);
    cued_pat_glob.set(init_cued);
    pending_pat_glob.set(NO_PENDING);
    turnaround_glob.set(value_to_turnaround_bars(saved_shift));
    fill_stage_glob.set(value_to_fill_stage(saved_fn));

    leds.set(
        0,
        Led::Bottom,
        PATTERN_COLORS[init_cued],
        Brightness::Low,
    );
    if mute {
        leds.unset(0, Led::Button);
        leds.unset(0, Led::Top);
    } else {
        leds.set(
            0,
            Led::Button,
            PATTERN_COLORS[init_active],
            LED_BRIGHTNESS,
        );
    }

    // 1. Clock & Phase-Locked Pattern/Fill Execution Task
    let fut_clock = async {
        let mut note_on = false;
        let mut gate_on = false;
        let gate_step = (STEP_TICKS * gatel / 100).clamp(1, STEP_TICKS - 1);

        loop {
            match clock.wait_for_event(ClockDivision::_1).await {
                ClockEvent::Reset | ClockEvent::Stop => {
                    clock_running_glob.set(false);
                    if note_on {
                        midi.send_note_off(note).await;
                        note_on = false;
                    }
                    if gate_on {
                        out_jack.set_low().await;
                        gate_on = false;
                    }

                    // If a pattern was armed before stop/reset, commit it cleanly now
                    let pending = pending_pat_glob.get();
                    if pending < PATTERNS.len() {
                        active_pat_glob.set(pending);
                        pending_pat_glob.set(NO_PENDING);
                        storage.modify_and_save(|s| {
                            s.active_pat = pending as u8;
                        });
                        if !glob_muted.get() {
                            leds.set(0, Led::Button, PATTERN_COLORS[pending], LED_BRIGHTNESS);
                        }
                    }

                    if glob_latch_layer.get() == LatchLayer::Main {
                        leds.set(
                            0,
                            Led::Top,
                            PATTERN_COLORS[active_pat_glob.get()],
                            Brightness::Off,
                        );
                    }
                }
                ClockEvent::Tick(tick) => {
                    let clkn = tick as u32;
                    clock_running_glob.set(true);
                    last_tick_glob.set(clkn);

                    let muted = glob_muted.get();

                    // Start of a 16th-note step (phase-locked to global tick)
                    if clkn.is_multiple_of(STEP_TICKS) {
                        let global_step = clkn / STEP_TICKS;
                        let step = (global_step as usize) % PATTERN_LEN;
                        let bar = global_step / (PATTERN_LEN as u32);

                        // Commit armed pattern on the exact bar downbeat (step 0)
                        if step == 0 {
                            let pending = pending_pat_glob.get();
                            if pending < PATTERNS.len() {
                                active_pat_glob.set(pending);
                                pending_pat_glob.set(NO_PENDING);
                                storage.modify_and_save(|s| {
                                    s.active_pat = pending as u8;
                                });
                                if !muted {
                                    leds.set(
                                        0,
                                        Led::Button,
                                        PATTERN_COLORS[pending],
                                        LED_BRIGHTNESS,
                                    );
                                }
                            }
                        }

                        let active_pat = active_pat_glob.get();
                        let cued_pat = cued_pat_glob.get();
                        let pending_pat = pending_pat_glob.get();
                        let turnaround_bars = turnaround_glob.get();
                        let max_fill_code = fill_stage_glob.get();

                        let is_turnaround_bar = turnaround_bars > 0
                            && (bar % turnaround_bars) == (turnaround_bars - 1);

                        let step_code = PATTERNS[active_pat][step];
                        let is_main_hit = step_code == 1;
                        let is_fill_hit =
                            is_turnaround_bar && step_code >= 2 && step_code <= max_fill_code;
                        let should_fire = is_main_hit || is_fill_hit;

                        if !muted && should_fire {
                            let vel = if is_main_hit {
                                MAIN_VELOCITY
                            } else {
                                FILL_VELOCITY
                            };

                            midi.send_note_on(note, vel).await;
                            note_on = true;

                            let fire_jack = match jack_mode {
                                JACK_OUT_ALL => true,
                                JACK_OUT_BASE => is_main_hit,
                                JACK_OUT_FILLS => is_fill_hit,
                                _ => false,
                            };

                            if fire_jack {
                                out_jack.set_high().await;
                                gate_on = true;
                            }

                            if glob_latch_layer.get() == LatchLayer::Main {
                                let hit_bright = if is_main_hit {
                                    Brightness::High
                                } else {
                                    Brightness::Mid
                                };
                                leds.set(0, Led::Top, PATTERN_COLORS[active_pat], hit_bright);
                            }
                        }

                        // Bottom LED status:
                        // - Armed for next bar: flashes White on beats, cued color on off-steps
                        // - Cued (not yet armed): pulses cued color on beats
                        // - Active: steady low in active pattern color
                        if pending_pat < PATTERNS.len() {
                            if step.is_multiple_of(4) {
                                leds.set(0, Led::Bottom, Color::White, Brightness::High);
                            } else {
                                leds.set(
                                    0,
                                    Led::Bottom,
                                    PATTERN_COLORS[pending_pat],
                                    Brightness::Mid,
                                );
                            }
                        } else if cued_pat != active_pat {
                            let blink_bright = if step.is_multiple_of(4) {
                                Brightness::High
                            } else {
                                Brightness::Low
                            };
                            leds.set(0, Led::Bottom, PATTERN_COLORS[cued_pat], blink_bright);
                        } else {
                            leds.set(0, Led::Bottom, PATTERN_COLORS[active_pat], Brightness::Low);
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
                            leds.set(
                                0,
                                Led::Top,
                                PATTERN_COLORS[active_pat_glob.get()],
                                Brightness::Off,
                            );
                        }
                    }

                    // Layer LED meters while holding Shift (Alt) or Button+Fader (Third)
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

    // 2. Button Task:
    // - Short press (no Shift, no Fn+Fader movement, no long press): Mute / Unmute toggle
    // - Shift + Short press: Arm cued pattern for next bar downbeat (or commit immediately if stopped)
    let fut_button = async {
        loop {
            let (chan, shift) = buttons.wait_for_any_down().await;
            if chan == 0 {
                long_press_fired.set(false);
                fader_at_down.set(fader.get_value());

                if shift {
                    alt_layer_used.set(false);
                    buttons.wait_for_up(0).await;

                    if !alt_layer_used.get() && !long_press_fired.get() {
                        let cued = cued_pat_glob.get();
                        let active = active_pat_glob.get();

                        if cued == active {
                            pending_pat_glob.set(NO_PENDING);
                            leds.set(0, Led::Bottom, PATTERN_COLORS[active], Brightness::Low);
                        } else if !clock_running_glob.get()
                            || (last_tick_glob.get() % BAR_TICKS) <= 1
                        {
                            active_pat_glob.set(cued);
                            pending_pat_glob.set(NO_PENDING);
                            storage.modify_and_save(|s| {
                                s.active_pat = cued as u8;
                            });
                            leds.set(0, Led::Bottom, PATTERN_COLORS[cued], Brightness::Low);
                            if !glob_muted.get() {
                                leds.set(0, Led::Button, PATTERN_COLORS[cued], LED_BRIGHTNESS);
                            }
                        } else {
                            pending_pat_glob.set(cued);
                            leds.set(0, Led::Bottom, Color::White, Brightness::High);
                        }
                    }
                } else {
                    third_layer_used.set(false);
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
                            let active = active_pat_glob.get();
                            leds.set(0, Led::Button, PATTERN_COLORS[active], LED_BRIGHTNESS);
                        }
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
                alt_layer_used.set(true);
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
                        let cued_idx = value_to_pattern(new_value);
                        cued_pat_glob.set(cued_idx);
                        storage.modify_and_save(|s| s.fader_saved = new_value);

                        // If already armed, update the armed target to follow the fader
                        if pending_pat_glob.get() < PATTERNS.len() {
                            if cued_idx == active_pat_glob.get() {
                                pending_pat_glob.set(NO_PENDING);
                            } else {
                                pending_pat_glob.set(cued_idx);
                            }
                        }

                        let bright = if cued_idx != active_pat_glob.get() {
                            Brightness::High
                        } else {
                            Brightness::Low
                        };
                        leds.set(0, Led::Bottom, PATTERN_COLORS[cued_idx], bright);
                    }
                    LatchLayer::Alt => {
                        let bars = value_to_turnaround_bars(new_value);
                        turnaround_glob.set(bars);
                        storage.modify_and_save(|s| s.shift_fader_saved = new_value);
                    }
                    LatchLayer::Third => {
                        let stage = value_to_fill_stage(new_value);
                        fill_stage_glob.set(stage);
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
                    let (saved_active, saved_fader, saved_shift, saved_fn, mute) =
                        storage.query(|s| {
                            (
                                s.active_pat as usize,
                                s.fader_saved,
                                s.shift_fader_saved,
                                s.fn_fader_saved,
                                s.muted,
                            )
                        });

                    let active_idx = saved_active.min(PATTERNS.len() - 1);
                    let cued_idx = value_to_pattern(saved_fader);

                    glob_muted.set(mute);
                    active_pat_glob.set(active_idx);
                    cued_pat_glob.set(cued_idx);
                    pending_pat_glob.set(NO_PENDING);
                    turnaround_glob.set(value_to_turnaround_bars(saved_shift));
                    fill_stage_glob.set(value_to_fill_stage(saved_fn));

                    leds.set(0, Led::Bottom, PATTERN_COLORS[cued_idx], Brightness::Low);

                    if mute {
                        leds.unset(0, Led::Button);
                        leds.unset(0, Led::Top);
                        out_jack.set_low().await;
                    } else {
                        leds.set(
                            0,
                            Led::Button,
                            PATTERN_COLORS[active_idx],
                            LED_BRIGHTNESS,
                        );
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
            } else if buttons.is_button_pressed(0) && buttons.is_shift_pressed() {
                if fader.get_value().abs_diff(fader_at_down.get()) > FADER_MOVE_THRESH {
                    alt_layer_used.set(true);
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
                    leds.set(
                        0,
                        Led::Top,
                        PATTERN_COLORS[active_pat_glob.get()],
                        Brightness::Off,
                    );
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