use embassy_futures::{
    join::{join, join3, join5},
    select::{select, select3},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use heapless::Vec;
use serde::{Deserialize, Serialize};

use libfp::{
    ext::FromValue,
    latch::LatchLayer,
    quantizer::Pitch,
    utils::attenuate_bipolar,
    AppIcon, Brightness, ClockDivision, Color, Config, MidiChannel, MidiNote, MidiOut, Note, Param, Range, Value,
    VoltPerOct, APP_MAX_PARAMS,
};

use crate::app::{
    App, AppParams, AppStorage, ClockEvent, Led, LedMode, ManagedStorage, ParamStore, SceneEvent,
};

use self::coltrane_geo::{
    arp_order, build_chord_voice_led, build_cycle, center_from_app, feel_swing_ticks,
    feel_velocity, function_hue, interval_color, step_div_mult, tritone_sub_root, ChordQuality,
    Motion, MOTION_LABELS,
};

pub const CHANNELS: usize = 1;
pub const PARAMS: usize = 16;

const BUTTON_DUCK_MS: u16 = 25;
const SOUNDING_CAP: usize = 8;
/// Hold off the periodic button paint so LedMode::Flash can finish.
const BUTTON_FLASH_MS: u16 = 550;
/// Reverse gesture LED feedback (white<->none).
const REVERSE_FADE_MS: u16 = 500;
/// LED loop period; every ms countdown steps by this.
const LED_STEP_MS: u16 = 8;

/// CV out parking value while muted: 0 V, which is mid-scale on a bipolar range.
fn cv_idle(range: Range) -> u16 {
    if range.is_bipolar() {
        2048
    } else {
        0
    }
}

const DIV_LABELS: &[&str] = &[
    "1/1", "1/2", "1/4", "1/4t", "1/8", "1/8t", "1/16", "1/16t", "1/32", "1/32t", "1/64t",
];
const RESOLUTION: [u32; 11] = [96, 48, 24, 16, 12, 8, 6, 4, 3, 2, 1];

const JACK_OUT: usize = 0;
const JACK_IN_DENSITY: usize = 1;
const JACK_IN_RESET: usize = 2;
const JACK_COUNT: usize = 3;
const TRIG_HIGH: u16 = 2458;

pub static CONFIG: Config<PARAMS> = Config::new(
    "Giant Steps",
    "Clock-driven chord sequencer over Coltrane tonal center geometry",
    Color::Blue,
    AppIcon::NoteGrid,
)
.add_param(Param::MidiOut)
.add_param(Param::MidiChannel {
    name: "MIDI Channel",
})
.add_param(Param::MidiNote { name: "Root" })
.add_param(Param::Enum {
    name: "Division",
    variants: DIV_LABELS,
})
.add_param(Param::Enum {
    name: "Interval",
    variants: &["Minor 3rd", "Major 3rd", "Perfect 4th", "Tritone"],
})
.add_param(Param::Enum {
    name: "Voicing",
    variants: &["Triad", "7th", "Drop-2", "Quartal"],
})
.add_param(Param::Enum {
    name: "Direction",
    variants: &["Forward", "Reverse", "Pendulum", "Random"],
})
.add_param(Param::i32 {
    name: "Velocity",
    min: 1,
    max: 127,
})
.add_param(Param::Color {
    name: "Color",
    variants: &[
        Color::Blue,
        Color::Green,
        Color::Orange,
        Color::Cyan,
        Color::Rose,
        Color::Violet,
        Color::Pink,
        Color::Yellow,
    ],
})
.add_param(Param::Enum {
    name: "Jack",
    variants: &["CV Out", "CV In Density", "CV In Reset"],
})
.add_param(Param::Range {
    name: "Range",
    variants: &[Range::_0_10V, Range::_Neg5_5V],
})
.add_param(Param::VoltPerOct)
.add_param(Param::i32 {
    name: "CV Att",
    min: 0,
    max: 400,
})
.add_param(Param::bool {
    name: "Follow device tonic",
})
.add_param(Param::bool {
    name: "Follow device scale",
})
.add_param(Param::Enum {
    name: "Motion",
    variants: MOTION_LABELS,
});

pub struct Params {
    midi_out: MidiOut,
    midi_channel: MidiChannel,
    root: MidiNote,
    division: usize,
    interval: usize,
    voicing: usize,
    direction: usize,
    velocity: i32,
    color: Color,
    jack: usize,
    range: Range,
    vpo: VoltPerOct,
    cv_att: i32,
    follow_tonic: bool,
    follow_scale: bool,
    motion: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            midi_out: MidiOut([true, false, false]),
            midi_channel: MidiChannel::default(),
            root: MidiNote::from(48),
            division: 4,
            interval: 1,
            voicing: 1,
            direction: 0,
            velocity: 100,
            color: Color::Blue,
            jack: JACK_OUT,
            range: Range::_0_10V,
            vpo: VoltPerOct::Standard,
            cv_att: 100,
            follow_tonic: true,
            follow_scale: false,
            motion: 0,
        }
    }
}

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.len() < PARAMS {
            return None;
        }
        Some(Self {
            midi_out: MidiOut::from_value(values[0]),
            midi_channel: MidiChannel::from_value(values[1]),
            root: MidiNote::from_value(values[2]),
            division: usize::from_value(values[3]).min(RESOLUTION.len() - 1),
            interval: usize::from_value(values[4]).min(3),
            voicing: usize::from_value(values[5]).min(3),
            direction: usize::from_value(values[6]).min(3),
            velocity: i32::from_value(values[7]).clamp(1, 127),
            color: Color::from_value(values[8]),
            jack: usize::from_value(values[9]).min(JACK_COUNT - 1),
            range: Range::from_value(values[10]),
            vpo: VoltPerOct::from_value(values[11]),
            cv_att: i32::from_value(values[12]).clamp(0, 400),
            follow_tonic: bool::from_value(values[13]),
            follow_scale: bool::from_value(values[14]),
            motion: usize::from_value(values[15]).min(3),
        })
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        let mut v = Vec::new();
        v.push(self.midi_out.into()).unwrap();
        v.push(self.midi_channel.into()).unwrap();
        v.push(self.root.into()).unwrap();
        v.push(self.division.into()).unwrap();
        v.push(self.interval.into()).unwrap();
        v.push(self.voicing.into()).unwrap();
        v.push(self.direction.into()).unwrap();
        v.push(self.velocity.into()).unwrap();
        v.push(self.color.into()).unwrap();
        v.push(self.jack.into()).unwrap();
        v.push(self.range.into()).unwrap();
        v.push(self.vpo.into()).unwrap();
        v.push(self.cv_att.into()).unwrap();
        v.push(self.follow_tonic.into()).unwrap();
        v.push(self.follow_scale.into()).unwrap();
        v.push(self.motion.into()).unwrap();
        v
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Storage {
    density_saved: u16,
    muted: bool,
    interval_idx: u8,
    feel: u16,
    time_fader: u16,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            density_saved: 2048,
            muted: false,
            interval_idx: 1,
            feel: 0,
            // Sentinel: never touched, so the Config Division sets the start.
            time_fader: u16::MAX,
        }
    }
}

impl AppStorage for Storage {}

fn att_from_pct(pct: i32) -> u16 {
    ((pct.clamp(0, 400) as u32 * 4095) / 100) as u16
}

fn interval_semitones(param: usize) -> u8 {
    match param {
        0 => 3,
        1 => 4,
        2 => 5,
        3 => 6,
        _ => 4,
    }
}

fn velocity_12bit(vel_7: i32) -> u16 {
    let v = vel_7.clamp(1, 127) as u32;
    ((v * 4095) / 127) as u16
}

fn density_from_fader(v: u16) -> u8 {
    ((v as u32 * 7) / 4096).min(6) as u8
}

/// Fader domain -> absolute division; bottom = 1/1, top = 1/64t.
fn div_from_fader(v: u16) -> u32 {
    RESOLUTION[((v as u32 * 11) / 4096).min(10) as usize]
}

/// Centre of the fader zone belonging to a division index.
fn fader_from_div_idx(idx: usize) -> u16 {
    ((idx as u32 * 4096 + 2048) / 11).min(4095) as u16
}

/// Hue sway per harmonic function: tonic sits on the center hue, its V and ii
/// step further away.
fn function_degrees(q: ChordQuality) -> u16 {
    match q {
        ChordQuality::Maj7 => 0,
        ChordQuality::Dom7 => 20,
        ChordQuality::Min7 => 40,
    }
}

/// Density (0..=6) as button brightness: dim but readable up to near full.
fn density_brightness(density: u8) -> u8 {
    (60 + (u16::from(density.min(6)) * 195) / 6) as u8
}

fn note_to_pitch(note: u8) -> Pitch {
    let octave = (note as i16 / 12) - 1;
    let pc = note % 12;
    Pitch {
        octave: octave as i8,
        note: Note::from(pc),
        raw: None,
    }
}

#[embassy_executor::task(pool_size = 16/CHANNELS)]
pub async fn wrapper(app: App<CHANNELS>, exit_signal: &'static Signal<NoopRawMutex, bool>) {
    let param_store = ParamStore::<Params>::new(app.app_id, app.layout_id, Params::default());
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
    let (
        midi_out,
        midi_chan,
        root_note,
        division,
        interval_param,
        voicing,
        direction,
        velocity,
        led_color,
        jack_param,
        range,
        vpo,
        cv_att,
        follow_tonic,
        _follow_scale,
        motion,
    ) = params.query(|p| {
        (
            p.midi_out,
            p.midi_channel,
            p.root,
            p.division,
            p.interval,
            p.voicing,
            p.direction,
            p.velocity,
            p.color,
            p.jack.min(JACK_COUNT - 1),
            p.range,
            p.vpo,
            att_from_pct(p.cv_att),
            p.follow_tonic,
            p.follow_scale,
            Motion::from_index(p.motion),
        )
    });

    let vel12 = velocity_12bit(velocity);

    let buttons = app.use_buttons();
    let faders = app.use_faders();
    let leds = app.use_leds();
    let mut clock = app.use_clock();
    let glob_ticks = app.make_global(u64::MAX);
    let die = app.use_die();
    // Only used to read the device scale/tonic via `get_scale()` — this app
    // doesn't quantize its own CV output, so range/vpo/bypass are unused.
    let quantizer = app.use_quantizer(Range::default(), VoltPerOct::default(), true);
    let midi = app.use_midi_output(midi_out, midi_chan, false);

    let out_jack = if jack_param == JACK_OUT {
        Some(app.make_out_jack(0, range).await)
    } else {
        None
    };
    let in_jack = if jack_param != JACK_OUT {
        Some(app.make_in_jack(0, Range::_Neg5_5V).await)
    } else {
        None
    };

    let (density0, muted0, feel0, time_fader0) =
        storage.query(|s| (s.density_saved, s.muted, s.feel, s.time_fader));
    let div_idx = division.min(RESOLUTION.len() - 1);
    // Untouched storage: Config Division is the power-on position.
    let (time_fader0, div0) = if time_fader0 == u16::MAX {
        (fader_from_div_idx(div_idx), RESOLUTION[div_idx])
    } else {
        (time_fader0, div_from_fader(time_fader0))
    };

    let glob_density = app.make_global(density0);
    let glob_feel = app.make_global(feel0);
    let glob_time_fader = app.make_global(time_fader0);
    // Config Interval is the start value; Shift+Long cycles from there.
    let glob_interval_idx = app.make_global(interval_param.min(3) as u8);
    let glob_btn_flash = app.make_global(0u16);
    let glob_rev_fade = app.make_global(0u16);
    let glob_rev_fade_up = app.make_global(false);
    let glob_div = app.make_global(div0);
    let glob_muted = app.make_global(muted0);
    let glob_latch = app.make_global(LatchLayer::Main);
    let glob_fader_moved = app.make_global(false);
    let glob_button_duck = app.make_global(0u16);
    let glob_center_idx = app.make_global(0u8);
    // Hue sway of the sounding chord's function, and the density the engine
    // actually used (fader plus CV offset).
    let glob_fn_deg = app.make_global(0u16);
    let glob_density_step = app.make_global(density_from_fader(density0));
    let glob_reset = app.make_global(false);
    let glob_reverse = app.make_global(false);
    let glob_cv_val = app.make_global(2047u16);
    let glob_fader_dirty = app.make_global(false);
    let long_press_fired = app.make_global(false);

    if muted0 {
        leds.unset(0, Led::Button);
    } else {
        leds.set(0, Led::Button, led_color, Brightness::Mid);
    }

    let fut_engine = async {
        let mut sounding: Vec<u8, SOUNDING_CAP> = Vec::new();
        let mut step: usize = 0;
        let mut pendulum_fwd = true;
        let mut last_seen = glob_ticks.get();
        let mut last_div_fire: u64 = u64::MAX;
        let mut stall_ms = 0u16;
        let mut prev_gate_high = false;
        let mut prev_muted = muted0;
        let mut pending_fire: Option<u64> = None;
        // Arp run of the current chord, still waiting for its tick.
        let mut pending_notes: Vec<(u64, u8), SOUNDING_CAP> = Vec::new();
        // Division boundaries the current chord still swallows (harmonic rhythm).
        let mut hold_boundaries: u32 = 0;
        let mut arp_vel: u16 = vel12;

        loop {
            app.delay_millis(2).await;

            // Polled, so mute silences a held chord even with the clock stopped.
            let muted = glob_muted.get();
            if muted != prev_muted {
                prev_muted = muted;
                if muted {
                    for n in sounding.iter() {
                        midi.send_note_off(MidiNote::from(*n)).await;
                    }
                    sounding.clear();
                    pending_fire = None;
                    pending_notes.clear();
                    if let Some(ref jack) = out_jack {
                        jack.set_value(cv_idle(range));
                    }
                }
            }

            if let Some(ref input) = in_jack {
                let raw = input.get_value();
                let in_val = attenuate_bipolar(raw, cv_att);
                glob_cv_val.set(in_val);
                if jack_param == JACK_IN_RESET {
                    let high = in_val >= TRIG_HIGH;
                    if high && !prev_gate_high {
                        glob_reset.set(true);
                    }
                    prev_gate_high = high;
                } else {
                    prev_gate_high = false;
                }
            }

            let t = glob_ticks.get();
            if t == last_seen {
                stall_ms = stall_ms.saturating_add(2);
                if stall_ms >= 250 && !sounding.is_empty() {
                    for n in sounding.iter() {
                        midi.send_note_off(MidiNote::from(*n)).await;
                    }
                    sounding.clear();
                    pending_fire = None;
                    pending_notes.clear();
                    hold_boundaries = 0;
                }
                continue;
            }
            stall_ms = 0;

            if t < last_seen {
                for n in sounding.iter() {
                    midi.send_note_off(MidiNote::from(*n)).await;
                }
                sounding.clear();
                pending_fire = None;
                pending_notes.clear();
                hold_boundaries = 0;
                last_seen = t;
                last_div_fire = u64::MAX;
                step = 0;
                continue;
            }

            let div = glob_div.get().max(1) as u64;
            let boundary = t - (t % div);
            last_seen = t;

            // Drain the arp run of the current chord.
            if muted {
                pending_notes.clear();
            } else {
                let mut i = 0;
                while i < pending_notes.len() {
                    let (at, n) = pending_notes[i];
                    if t < at {
                        i += 1;
                        continue;
                    }
                    pending_notes.swap_remove(i);
                    // Retrigger on the descent: off first so the note stays
                    // unique in `sounding`.
                    if let Some(pos) = sounding.iter().position(|&s| s == n) {
                        midi.send_note_off(MidiNote::from(n)).await;
                        sounding.swap_remove(pos);
                    }
                    midi.send_note_on(MidiNote::from(n), arp_vel).await;
                    let _ = sounding.push(n);
                }
            }

            let mut fire = false;
            if !(boundary == 0 && t < div) && boundary != last_div_fire {
                last_div_fire = boundary;
                if hold_boundaries > 0 {
                    hold_boundaries -= 1;
                } else {
                    // Feel shares its timing budget with the device-global swing.
                    let gs = app.global_swing();
                    // Parity comes off the clock grid, not the app's start.
                    let delay =
                        feel_swing_ticks(glob_feel.get(), div as u32, (boundary / div) as u32, gs)
                            as u64;
                    if delay == 0 {
                        fire = true;
                    } else {
                        pending_fire = Some(boundary + delay);
                    }
                }
            }
            if let Some(at) = pending_fire {
                if t >= at {
                    pending_fire = None;
                    fire = true;
                }
            }
            if !fire || muted {
                continue;
            }

            if glob_reset.get() {
                glob_reset.set(false);
                step = 0;
                pendulum_fwd = true;
            }

            let density_fader = if jack_param == JACK_IN_DENSITY {
                let base = glob_density.get();
                (base as i32 + glob_cv_val.get() as i32 - 2047).clamp(0, 4095) as u16
            } else {
                glob_density.get()
            };
            let density = density_from_fader(density_fader);

            let root_midi = follow_key::root(&quantizer, follow_tonic, root_note).await;
            let interval_semi = interval_semitones(glob_interval_idx.get() as usize);
            let cycle = build_cycle(interval_semi, density);
            let cycle_len = cycle.len();

            if cycle_len == 0 {
                continue;
            }

            let idx = step % cycle_len;
            let cs = cycle[idx];

            let chord_root = (root_midi as u16 + cs.root_offset as u16).min(127) as u8;
            let chord_root = tritone_sub_root(chord_root, cs.quality, motion, die.roll());
            let notes = build_chord_voice_led(chord_root, cs.quality, voicing, &sounding);

            for n in sounding.iter() {
                midi.send_note_off(MidiNote::from(*n)).await;
            }
            sounding.clear();
            pending_notes.clear();

            // Tonic of each center carries the accent, ii / V lighten.
            let vel = feel_velocity(
                vel12,
                glob_feel.get(),
                cs.quality == ChordQuality::Maj7,
                die.roll(),
            );
            arp_vel = vel;

            let mult = step_div_mult(motion, cs.quality);
            hold_boundaries = mult - 1 + cs.hold as u32;

            let order = if motion >= Motion::Sheets {
                arp_order(notes.len())
            } else {
                Vec::new()
            };
            // Spread the run over what is left of the step (swing may have
            // eaten into it); too short for a tick per note and it stays a
            // block chord.
            let spacing = if order.is_empty() {
                0
            } else {
                let window = ((boundary / div + mult as u64) * div).saturating_sub(t);
                window / order.len() as u64
            };

            if spacing == 0 {
                for &n in notes.iter() {
                    midi.send_note_on(MidiNote::from(n), vel).await;
                    let _ = sounding.push(n);
                }
            } else {
                for (slot, &i) in order.iter().enumerate() {
                    let n = notes[i as usize];
                    if slot == 0 {
                        midi.send_note_on(MidiNote::from(n), vel).await;
                        let _ = sounding.push(n);
                    } else {
                        let _ = pending_notes.push((t + slot as u64 * spacing, n));
                    }
                }
            }

            if let Some(ref jack) = out_jack {
                jack.set_value(note_to_pitch(chord_root).as_counts(range, vpo));
            }

            glob_center_idx.set(cs.center);
            glob_fn_deg.set(function_degrees(cs.quality));
            glob_density_step.set(density);
            glob_button_duck.set(BUTTON_DUCK_MS);

            let reversed = glob_reverse.get();
            let dir = if reversed {
                match direction {
                    0 => 1,
                    1 => 0,
                    d => d,
                }
            } else {
                direction
            };
            match dir {
                1 => {
                    if step == 0 {
                        step = cycle_len - 1;
                    } else {
                        step -= 1;
                    }
                }
                2 => {
                    if pendulum_fwd {
                        step += 1;
                        if step >= cycle_len {
                            step = cycle_len.saturating_sub(2);
                            pendulum_fwd = false;
                        }
                    } else if step == 0 {
                        step = 1.min(cycle_len - 1);
                        pendulum_fwd = true;
                    } else {
                        step -= 1;
                    }
                }
                3 => {
                    step = (die.roll() as usize) % cycle_len;
                }
                _ => {
                    step = (step + 1) % cycle_len;
                }
            }
        }
    };

    let fut_faders = async {
        let mut latch = app.make_latch(faders.get_value());
        loop {
            faders.wait_for_change_at(0).await;
            let layer = glob_latch.get();
            glob_fader_moved.set(true);

            let target = match layer {
                LatchLayer::Third => glob_time_fader.get(),
                LatchLayer::Alt => glob_feel.get(),
                _ => glob_density.get(),
            };

            if let Some(v) = latch.update(faders.get_value(), layer, target) {
                match layer {
                    LatchLayer::Main => {
                        glob_density.set(v);
                    }
                    LatchLayer::Alt => {
                        glob_feel.set(v);
                    }
                    LatchLayer::Third => {
                        glob_time_fader.set(v);
                        glob_div.set(div_from_fader(v));
                    }
                }
                glob_fader_dirty.set(true);
            }
        }
    };

    let fut_buttons = async {
        loop {
            let (_, down_shift) = buttons.wait_for_any_down().await;
            long_press_fired.set(false);
            glob_fader_moved.set(false);
            buttons.wait_for_up(0).await;

            if long_press_fired.get() || glob_fader_moved.get() {
                continue;
            }
            if down_shift {
                glob_reset.set(true);
            } else {
                glob_muted.set(!glob_muted.get());
                glob_fader_dirty.set(true);
            }
        }
    };

    let fut_long = async {
        loop {
            let (_, shift) = buttons.wait_for_any_long_press().await;
            long_press_fired.set(true);
            // Third layer is button held + fader moved; don't fire on those.
            if glob_fader_moved.get() {
                continue;
            }
            if shift {
                let idx = (glob_interval_idx.get() + 1) % 4;
                glob_interval_idx.set(idx);
                glob_fader_dirty.set(true);
                leds.set_mode(0, Led::Button, LedMode::Flash(interval_color(idx), Some(2)));
                glob_btn_flash.set(BUTTON_FLASH_MS);
            } else {
                let rev = !glob_reverse.get();
                glob_reverse.set(rev);
                glob_rev_fade_up.set(!rev);
                glob_rev_fade.set(REVERSE_FADE_MS);
            }
        }
    };

    let fut_persist = async {
        loop {
            app.delay_millis(400).await;
            if !glob_fader_dirty.get() {
                continue;
            }
            glob_fader_dirty.set(false);
            storage.modify_and_save(|st| {
                st.density_saved = glob_density.get();
                st.muted = glob_muted.get();
                st.interval_idx = glob_interval_idx.get();
                st.feel = glob_feel.get();
                st.time_fader = glob_time_fader.get();
            });
        }
    };

    let fut_leds = async {
        loop {
            app.delay_millis(8).await;

            let shift = buttons.is_shift_pressed();
            let pressed = buttons.is_button_pressed(0);
            let layer = if shift && !pressed {
                LatchLayer::Alt
            } else if pressed && !shift {
                LatchLayer::Third
            } else {
                LatchLayer::Main
            };
            glob_latch.set(layer);

            let flash_left = glob_btn_flash.get();
            if flash_left > 0 {
                glob_btn_flash.set(flash_left.saturating_sub(LED_STEP_MS));
            }

            // Reverse feedback: white<->none fade owns the button while it runs.
            let rev_fade = glob_rev_fade.get();
            if rev_fade > 0 {
                let elapsed = REVERSE_FADE_MS.saturating_sub(rev_fade);
                let ramp = if glob_rev_fade_up.get() {
                    ((elapsed as u32 * 255) / REVERSE_FADE_MS as u32) as u8
                } else {
                    (((REVERSE_FADE_MS - elapsed) as u32 * 255) / REVERSE_FADE_MS as u32) as u8
                };
                leds.set(0, Led::Button, Color::White, Brightness::Custom(ramp));
                glob_rev_fade.set(rev_fade.saturating_sub(LED_STEP_MS));
            }

            let duck_active = {
                let d = glob_button_duck.get();
                if d > 0 {
                    glob_button_duck.set(d.saturating_sub(8));
                    true
                } else {
                    false
                }
            };

            let muted = glob_muted.get();
            let center = glob_center_idx.get();
            let app_color = params.query(|p| p.color);
            // Whole 120 deg triad rotates with the Color param.
            let center_col = center_from_app(app_color, center);

            if !muted {
                leds.set(0, Led::Top, center_col, Brightness::Mid);
                leds.set(0, Led::Bottom, center_col, Brightness::Low);
                if rev_fade == 0 && flash_left == 0 {
                    // Hue = harmonic function, brightness = density; the trigger
                    // duck scales that brightness instead of replacing it.
                    let btn_col = function_hue(app_color, center, glob_fn_deg.get());
                    let mut b = density_brightness(glob_density_step.get());
                    if duck_active {
                        b = ((u16::from(b) * 2) / 5) as u8;
                    }
                    leds.set(0, Led::Button, btn_col, Brightness::Custom(b));
                }
            } else {
                leds.unset(0, Led::Top);
                leds.unset(0, Led::Bottom);
                if rev_fade == 0 && flash_left == 0 {
                    leds.unset(0, Led::Button);
                }
            }
        }
    };

    let fut_scene = async {
        loop {
            match app.wait_for_scene_event().await {
                SceneEvent::LoadScene(scene) => {
                    storage.load_from_scene(scene).await;
                    let (d, m, iv, f, tf) = storage.query(|s| {
                        (
                            s.density_saved,
                            s.muted,
                            s.interval_idx,
                            s.feel,
                            s.time_fader,
                        )
                    });
                    glob_density.set(d);
                    glob_muted.set(m);
                    glob_interval_idx.set(iv.min(3));
                    glob_feel.set(f);
                    let idx = params.query(|p| p.division).min(RESOLUTION.len() - 1);
                    let (tf, div) = if tf == u16::MAX {
                        (fader_from_div_idx(idx), RESOLUTION[idx])
                    } else {
                        (tf, div_from_fader(tf))
                    };
                    glob_time_fader.set(tf);
                    glob_div.set(div);
                }
                SceneEvent::SaveScene(scene) => storage.save_to_scene(scene).await,
            }
        }
    };


    let clock_drain = async {
        loop {
            match clock.wait_for_event(ClockDivision::_1).await {
                ClockEvent::Tick(tick) => {
                    glob_ticks.set(tick);
                }
                ClockEvent::Reset => {
                    glob_ticks.set(0);
                }
                _ => {}
            }
        }
    };

    join5(
        fut_engine,
        fut_faders,
        join3(fut_buttons, fut_long, fut_persist),
        fut_leds,
        join(fut_scene, clock_drain),
    )
    .await;
}

mod follow_key {
    use libfp::MidiNote;
    use midly::num::u7;

    use crate::app::Quantizer;

    pub async fn root(quantizer: &Quantizer, follow: bool, local_root: MidiNote) -> u8 {
        if follow {
            let (_, tonic) = quantizer.get_scale().await;
            retune(local_root, tonic as u8)
        } else {
            midi_u8(local_root)
        }
    }

    fn retune(root: MidiNote, pc: u8) -> u8 {
        let n = midi_u8(root);
        let target = (n - n % 12) + pc % 12;
        if target > 127 {
            target - 12
        } else {
            target
        }
    }

    fn midi_u8(note: MidiNote) -> u8 {
        u7::from(note).as_int()
    }
}

mod groove {
    /// Half of the device swing window in 24-PPQN ticks. Mirrors the private
    /// `SWING_HALF_INTERVAL` in [`crate::tasks::clock`]; keep both in sync.
    const DEVICE_SWING_HALF: u32 = 6;

    const DEVICE_SWING_DIRECTION_MIN: i8 = 8;

    /// Fraction of one grid step, in per-mille, that the device's global swing
    /// already displaces.
    pub fn device_swing_permille(div_ticks: u32, swing_amount: i8) -> u32 {
        if div_ticks > DEVICE_SWING_HALF || swing_amount == 0 {
            return 0;
        }
        (swing_amount.unsigned_abs() as u32 * 1000) / 50
    }

    /// Whether the app should flip which side of the grid it delays.
    pub fn device_swing_reverses(swing_amount: i8) -> bool {
        swing_amount <= -DEVICE_SWING_DIRECTION_MIN
    }
}

mod coltrane_geo {
//! Coltrane Changes geometry: tonal center cycles, approach patterns, chord
//! building. Shared by Giant Steps and Axis Matrix.

use super::groove::{device_swing_permille, device_swing_reverses};
use heapless::Vec;
use libfp::Color;
use smart_leds::RGB8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChordQuality {
    Maj7,
    Dom7,
    Min7,
}

/// How much the harmony is allowed to breathe. Each level adds to the one
/// below it; `Straight` is the untouched engine.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Motion {
    Straight,
    Rubato,
    Sheets,
    Free,
}

pub const MOTION_LABELS: &[&str] = &["Straight", "Rubato", "Sheets", "Free"];

impl Motion {
    pub fn from_index(idx: usize) -> Self {
        match idx {
            1 => Motion::Rubato,
            2 => Motion::Sheets,
            3 => Motion::Free,
            _ => Motion::Straight,
        }
    }
}

/// How many divisions a chord of this function occupies. The tonic that
/// resolves a center leans, the approach chords keep passing.
pub fn step_div_mult(motion: Motion, quality: ChordQuality) -> u32 {
    if motion >= Motion::Rubato && quality == ChordQuality::Maj7 {
        2
    } else {
        1
    }
}

/// Tritone substitution: roughly one dominant in three gets the root a tritone
/// away, quality untouched. Tonics and approach ii chords are never swapped.
pub fn tritone_sub_root(root_midi: u8, quality: ChordQuality, motion: Motion, roll: u16) -> u8 {
    if motion < Motion::Free || quality != ChordQuality::Dom7 || roll >= 1365 {
        return root_midi;
    }
    if root_midi <= 121 {
        root_midi + 6
    } else {
        root_midi - 6
    }
}

/// Ascending-then-descending index run over a chord of `len` notes.
pub fn arp_order(len: usize) -> Vec<u8, 8> {
    let mut out: Vec<u8, 8> = Vec::new();
    for i in 0..len {
        let _ = out.push(i as u8);
    }
    for i in (1..len.saturating_sub(1)).rev() {
        let _ = out.push(i as u8);
    }
    out
}

#[derive(Clone, Copy)]
pub struct CycleStep {
    pub center: u8,
    pub root_offset: u8,
    pub quality: ChordQuality,
    /// Extra division boundaries this chord swallows because the following
    /// slots on the fixed 9-slot grid are empty (it sustains through them).
    pub hold: u8,
}

/// Build the ordered Coltrane cycle on a fixed 9-slot metric grid.
///
/// `interval` is the semitone gap between tonal centers (3 = minor 3rd,
/// 4 = major 3rd, etc.). The grid is always ii(c), V(c), I(c) for each of the
/// three centers. `density` (0..=6) fades approach chords in one at a time:
/// I is always present, V(c) appears once `density >= c + 1`, ii(c) once
/// `density >= 4 + c`. That yields cycle lengths 3,4,5,6,7,8,9.
///
/// Empty slots do not shorten the cycle: they are folded cyclically into the
/// `hold` of the preceding emitted chord (leading empties wrap onto the last
/// chord), so `sum(1 + hold) == 9` always.
pub fn build_cycle(interval: u8, density: u8) -> Vec<CycleStep, 9> {
    let mut out: Vec<CycleStep, 9> = Vec::new();
    let interval = interval.max(1);
    let density = density.min(6);
    let mut leading_empty = 0u8;

    for c in 0u8..3 {
        let center_offset = (c as u16 * interval as u16 % 12) as u8;

        let slots = [
            (
                density >= 4 + c,
                (center_offset + 2) % 12,
                ChordQuality::Min7,
            ),
            (density > c, (center_offset + 7) % 12, ChordQuality::Dom7),
            (true, center_offset, ChordQuality::Maj7),
        ];

        for (present, root_offset, quality) in slots {
            if present {
                let _ = out.push(CycleStep {
                    center: c,
                    root_offset,
                    quality,
                    hold: 0,
                });
            } else if let Some(last) = out.last_mut() {
                last.hold += 1;
            } else {
                leading_empty += 1;
            }
        }
    }

    if let Some(last) = out.last_mut() {
        last.hold += leading_empty;
    }
    out
}

/// Build MIDI note numbers for a chord.
///
/// Voicings: 0 = close triad, 1 = close 7th, 2 = open/drop-2 7th,
/// 3 = quartal (stacked 4ths).
pub fn build_coltrane_chord(root_midi: u8, quality: ChordQuality, voicing: usize) -> Vec<u8, 8> {
    let mut out: Vec<u8, 8> = Vec::new();
    let r = root_midi as i16;

    let intervals: &[i16] = match (quality, voicing) {
        (_, 3) => &[0, 5, 10, 15],
        (ChordQuality::Maj7, 0) => &[0, 4, 7],
        (ChordQuality::Dom7, 0) => &[0, 4, 7],
        (ChordQuality::Min7, 0) => &[0, 3, 7],
        (ChordQuality::Maj7, 1) => &[0, 4, 7, 11],
        (ChordQuality::Dom7, 1) => &[0, 4, 7, 10],
        (ChordQuality::Min7, 1) => &[0, 3, 7, 10],
        (ChordQuality::Maj7, _) => &[0, 7, 11, 16],
        (ChordQuality::Dom7, _) => &[0, 7, 10, 16],
        (ChordQuality::Min7, _) => &[0, 7, 10, 15],
    };

    for &iv in intervals {
        let n = r + iv;
        if (0..=127).contains(&n) {
            let _ = out.push(n as u8);
        }
    }
    out
}

/// Chord voiced to minimise movement from the previous voicing.
///
/// Candidates are whole-chord octave shifts crossed with inversions; each is
/// scored by the total semitone distance to the nearest note of `prev`. Empty
/// `prev` falls back to `build_coltrane_chord`.
pub fn build_chord_voice_led(
    root_midi: u8,
    quality: ChordQuality,
    voicing: usize,
    prev: &[u8],
) -> Vec<u8, 8> {
    let base = build_coltrane_chord(root_midi, quality, voicing);
    if prev.is_empty() || base.is_empty() {
        return base;
    }

    let mut best = base.clone();
    let mut best_cost = u32::MAX;
    let mut cand: Vec<u8, 8> = Vec::new();

    // Neutral candidate comes first so ties keep the plain voicing.
    for &oct in &[0i16, -12, 12, -24, 24] {
        for inv in 0..base.len() {
            cand.clear();
            let mut fits = true;
            for (i, &n) in base.iter().enumerate() {
                let v = n as i16 + oct + if i < inv { 12 } else { 0 };
                if !(0..=127).contains(&v) {
                    fits = false;
                    break;
                }
                let _ = cand.push(v as u8);
            }
            if !fits {
                continue;
            }

            let mut cost = 0u32;
            for &n in cand.iter() {
                let mut d = u32::MAX;
                for &p in prev {
                    d = d.min(u32::from((n as i16 - p as i16).unsigned_abs()));
                }
                cost += d;
            }
            if cost < best_cost {
                best_cost = cost;
                best = cand.clone();
            }
        }
    }
    best
}

/// Feel-scaled velocity (12-bit). `feel` is 0..=4095; at 0 the base velocity is
/// returned untouched. `strong` marks the tonic of a center, which gets
/// accented while approach chords lighten. `roll` is a 0..=4095 die roll
/// driving the humanised variation.
pub fn feel_velocity(base_vel12: u16, feel: u16, strong: bool, roll: u16) -> u16 {
    if feel == 0 {
        return base_vel12;
    }
    let b = base_vel12 as i32;
    let s = (feel.min(4095) as i32 * 255) / 4095;
    // Per-mille of the base velocity at full Feel.
    let shape = if strong { 400 } else { -500 };
    let mut v = b + (b * shape * s) / (1000 * 255);
    let jitter = ((roll.min(4095) as i32 * 2000) / 4095) - 1000;
    let jb = (b * jitter) / 1000;
    v += (jb * 8 * s) / (100 * 255);
    v.clamp(1, 4095) as u16
}

/// MPC-style swing delay in clock ticks for `step`: one parity gets pushed back
/// by up to a third of the division at full Feel. That third is a budget shared
/// with the device clock — whatever the global swing already displaces is
/// subtracted — and a negative `swing_amount` flips which parity is delayed so
/// Feel leans with the clock instead of against it.
pub fn feel_swing_ticks(feel: u16, div_ticks: u32, step: u32, swing_amount: i8) -> u32 {
    if feel == 0 || div_ticks < 2 {
        return 0;
    }
    let delay_this = if device_swing_reverses(swing_amount) {
        step.is_multiple_of(2)
    } else {
        !step.is_multiple_of(2)
    };
    if !delay_this {
        return 0;
    }
    let budget_permille = 333u32.saturating_sub(device_swing_permille(div_ticks, swing_amount));
    if budget_permille == 0 {
        return 0;
    }
    // div_ticks <= 96, budget <= 333, feel <= 4095 → well inside u32.
    let d = (div_ticks * budget_permille * feel.min(4095) as u32) / (4095 * 1000);
    d.min(div_ticks - 1)
}

/// Feedback color for the Interval cycle gesture, one per index.
pub fn interval_color(idx: u8) -> Color {
    match idx % 4 {
        0 => Color::Cyan,
        1 => Color::Green,
        2 => Color::Yellow,
        _ => Color::Rose,
    }
}

/// Fixed RGB triad (legacy): Blue / Green / Orange at ~120 deg.
pub fn center_color(center_idx: u8) -> (u8, u8, u8) {
    match center_idx % 3 {
        0 => (40, 80, 220),
        1 => (30, 200, 80),
        _ => (230, 120, 20),
    }
}

/// Tonal-center color: rotate the 120 deg triad so center 0 matches `base` hue.
/// Centers stay equally spaced (+0 / +120 / +240) on the wheel.
pub fn center_from_app(base: Color, center_idx: u8) -> Color {
    let RGB8 { r, g, b } = RGB8::from(base);
    let (h, s, v) = rgb_to_hsv(r, g, b);
    // Near-white / grey: fall back to fixed triad so centers stay distinct.
    if s < 20 {
        let (cr, cg, cb) = center_color(center_idx);
        return Color::Custom(cr, cg, cb);
    }
    let h2 = (h + u16::from(center_idx % 3) * 120) % 360;
    let (nr, ng, nb) = hsv_to_rgb(h2, s.max(140), v.max(160));
    Color::Custom(nr, ng, nb)
}

/// Tonal-center color swayed by `degrees` on the wheel, to encode how far the
/// sounding chord sits from its center's own tonic.
pub fn function_hue(base: Color, center_idx: u8, degrees: u16) -> Color {
    let RGB8 { r, g, b } = RGB8::from(base);
    let (h, s, v) = rgb_to_hsv(r, g, b);
    if s < 20 {
        let (cr, cg, cb) = center_color(center_idx);
        return Color::Custom(cr, cg, cb);
    }
    let h2 = (h + u16::from(center_idx % 3) * 120 + degrees) % 360;
    let (nr, ng, nb) = hsv_to_rgb(h2, s.max(140), v.max(160));
    Color::Custom(nr, ng, nb)
}

fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (u16, u8, u8) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let v = max;
    if max == 0 {
        return (0, 0, 0);
    }
    let d = max - min;
    let s = ((u16::from(d) * 255) / u16::from(max)) as u8;
    if d == 0 {
        return (0, 0, v);
    }
    let (r, g, b, max, d) = (
        i32::from(r),
        i32::from(g),
        i32::from(b),
        i32::from(max),
        i32::from(d),
    );
    let h = if max == r {
        ((g - b) * 60) / d
    } else if max == g {
        120 + ((b - r) * 60) / d
    } else {
        240 + ((r - g) * 60) / d
    };
    ((h.rem_euclid(360)) as u16, s, v)
}

fn hsv_to_rgb(h: u16, s: u8, v: u8) -> (u8, u8, u8) {
    if s == 0 {
        return (v, v, v);
    }
    let h = h % 360;
    let sector = h / 60;
    let f = h % 60;
    let v = u16::from(v);
    let s = u16::from(s);
    let p = v * (255 - s) / 255;
    let q = v * (255 - (s * f) / 60) / 255;
    let t = v * (255 - (s * (60 - f)) / 60) / 255;
    let (r, g, b) = match sector {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    (r as u8, g as u8, b as u8)
}

/// Brightness (0-255) that peaks when fader position aligns with a center and
/// dims as it drifts away. `fader_pos` is 0-4095, mapped across `num_centers`.
pub fn center_brightness(fader_pos: u16, center_idx: u8, num_centers: u8) -> u8 {
    let nc = num_centers.max(1) as u32;
    let center_pos = (center_idx as u32 * 4095) / nc.max(1);
    let dist = (fader_pos as i32 - center_pos as i32).unsigned_abs();
    let span = 4095u32 / nc;
    if dist >= span {
        0
    } else {
        (255 * (span - dist) / span) as u8
    }
}
}
