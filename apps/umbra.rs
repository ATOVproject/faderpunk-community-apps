//! Umbra — bass shadow companion to Contura.
//!
//! Same latch-shell UX as Contura, but the pitch engine favours root, fifth,
//! and approach motion within a wander budget from the anchor. Hybrid density
//! adds ghost passes from mid-fader upward. Shared scale sets via
//! `contura_scales`. Optional follow of device tonic / scale.

use embassy_futures::{
    join::{join3, join5},
    select::{select, select3},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use heapless::Vec;
use midly::num::u7;
use serde::{Deserialize, Serialize};

use libfp::{
    ext::FromValue,
    latch::LatchLayer,
    quantizer::Pitch,
    utils::{attenuate_bipolar, split_unsigned_value},
    AppIcon, Brightness, ClockDivision, Color, Config, MidiChannel, MidiNote, MidiOut, Note, Param,
    Range, Value, VoltPerOct, APP_MAX_PARAMS,
};

use crate::app::{
    App, AppParams, AppStorage, ClockEvent, Die, Led, ManagedStorage, ParamStore, SceneEvent,
};

use self::contura_scales::{
    build_pool, clamp_scale, follow_mask_tonic, next_scale, prev_scale, POOL_CAP, SCALE_COUNT,
    SCALE_LABELS,
};

pub const CHANNELS: usize = 1;
pub const PARAMS: usize = 12;

const LED_BRIGHTNESS: Brightness = Brightness::Mid;
const OCTAVE_BLINK_MS: u16 = 250;
const BUTTON_DUCK_MS: u16 = 80;

const JACK_OUT: usize = 0;
const JACK_IN_DENSITY: usize = 1;
const JACK_IN_INTERVAL: usize = 2;
const JACK_IN_RESET: usize = 3;
const JACK_IN_PHRASE: usize = 4;
const JACK_COUNT: usize = 5;
const TRIG_HIGH: u16 = 2458;

fn signal_brightness(value: u16, bipolar: bool) -> Brightness {
    const MIN: u8 = 110;
    let raw = if bipolar {
        ((value.min(4095) as i32 - 2047).unsigned_abs() / 8).min(255) as u8
    } else {
        (value.min(4095) / 16) as u8
    };
    let span = 255u16 - MIN as u16;
    Brightness::Custom(MIN + ((raw as u16 * span) / 255) as u8)
}

const MIN_PHRASE: u8 = 3;
const MAX_PHRASE: u8 = 28;

const RESOLUTION: [u32; 13] = [384, 192, 96, 48, 24, 16, 12, 8, 6, 4, 3, 2, 1];
const DIV_LABELS: &[&str] = &[
    "4/1", "2/1", "1/1", "1/2", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/32T",
    "1/64T",
];

const OCT_COLORS: [Color; 2] = [Color::Blue, Color::Violet];

const SET_COLORS: [Color; 8] = [
    Color::Violet,
    Color::Cyan,
    Color::Rose,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Pink,
    Color::Orange,
];

/// Bass motion biases keyed to each scale label — parallel to Contura’s ScaleFeel.
#[derive(Clone, Copy)]
struct BassFeel {
    density_bias: i16,
    root_weight: u16,
    fifth_weight: u16,
    approach_chance: u16,
    ghost_chance: u16,
    octave_punch: u16,
    chromatic_spice: u16,
    sustain_bias: i16,
}

const FEEL_NEUTRAL: BassFeel = BassFeel {
    density_bias: 0,
    root_weight: 2200,
    fifth_weight: 1400,
    approach_chance: 500,
    ghost_chance: 400,
    octave_punch: 350,
    chromatic_spice: 120,
    sustain_bias: 0,
};

const BASS_FEELS: [BassFeel; SCALE_COUNT] = [
    BassFeel {
        density_bias: 150,
        root_weight: 2400,
        fifth_weight: 1200,
        approach_chance: 450,
        ghost_chance: 350,
        octave_punch: 300,
        chromatic_spice: 80,
        sustain_bias: 100,
    },
    BassFeel {
        density_bias: 100,
        root_weight: 2000,
        fifth_weight: 1600,
        approach_chance: 650,
        ghost_chance: 500,
        octave_punch: 280,
        chromatic_spice: 100,
        sustain_bias: 150,
    },
    BassFeel {
        density_bias: -80,
        root_weight: 2600,
        fifth_weight: 900,
        approach_chance: 750,
        ghost_chance: 300,
        octave_punch: 250,
        chromatic_spice: 200,
        sustain_bias: 200,
    },
    BassFeel {
        density_bias: 250,
        root_weight: 1800,
        fifth_weight: 1800,
        approach_chance: 550,
        ghost_chance: 450,
        octave_punch: 450,
        chromatic_spice: 90,
        sustain_bias: -100,
    },
    BassFeel {
        density_bias: -50,
        root_weight: 2500,
        fifth_weight: 1100,
        approach_chance: 600,
        ghost_chance: 380,
        octave_punch: 320,
        chromatic_spice: 150,
        sustain_bias: 250,
    },
    BassFeel {
        density_bias: 300,
        root_weight: 2800,
        fifth_weight: 800,
        approach_chance: 400,
        ghost_chance: 550,
        octave_punch: 200,
        chromatic_spice: 60,
        sustain_bias: -80,
    },
    BassFeel {
        density_bias: 200,
        root_weight: 2300,
        fifth_weight: 1300,
        approach_chance: 500,
        ghost_chance: 480,
        octave_punch: 280,
        chromatic_spice: 110,
        sustain_bias: 50,
    },
    BassFeel {
        density_bias: 180,
        root_weight: 2100,
        fifth_weight: 1500,
        approach_chance: 700,
        ghost_chance: 520,
        octave_punch: 350,
        chromatic_spice: 180,
        sustain_bias: -50,
    },
    BassFeel {
        density_bias: 220,
        root_weight: 2200,
        fifth_weight: 1400,
        approach_chance: 480,
        ghost_chance: 420,
        octave_punch: 300,
        chromatic_spice: 70,
        sustain_bias: 0,
    },
    BassFeel {
        density_bias: 280,
        root_weight: 2000,
        fifth_weight: 1600,
        approach_chance: 520,
        ghost_chance: 460,
        octave_punch: 320,
        chromatic_spice: 85,
        sustain_bias: -30,
    },
    BassFeel {
        density_bias: 120,
        root_weight: 2400,
        fifth_weight: 1200,
        approach_chance: 620,
        ghost_chance: 380,
        octave_punch: 260,
        chromatic_spice: 130,
        sustain_bias: 120,
    },
    BassFeel {
        density_bias: 160,
        root_weight: 2600,
        fifth_weight: 1000,
        approach_chance: 580,
        ghost_chance: 340,
        octave_punch: 240,
        chromatic_spice: 160,
        sustain_bias: 180,
    },
    BassFeel {
        density_bias: 140,
        root_weight: 2100,
        fifth_weight: 1500,
        approach_chance: 640,
        ghost_chance: 400,
        octave_punch: 290,
        chromatic_spice: 140,
        sustain_bias: 80,
    },
    BassFeel {
        density_bias: 100,
        root_weight: 2300,
        fifth_weight: 1300,
        approach_chance: 680,
        ghost_chance: 360,
        octave_punch: 270,
        chromatic_spice: 170,
        sustain_bias: 100,
    },
    BassFeel {
        density_bias: 200,
        root_weight: 2200,
        fifth_weight: 1400,
        approach_chance: 720,
        ghost_chance: 420,
        octave_punch: 310,
        chromatic_spice: 220,
        sustain_bias: 60,
    },
    BassFeel {
        density_bias: 190,
        root_weight: 2150,
        fifth_weight: 1450,
        approach_chance: 700,
        ghost_chance: 410,
        octave_punch: 300,
        chromatic_spice: 200,
        sustain_bias: 70,
    },
    BassFeel {
        density_bias: 170,
        root_weight: 2250,
        fifth_weight: 1350,
        approach_chance: 660,
        ghost_chance: 390,
        octave_punch: 285,
        chromatic_spice: 190,
        sustain_bias: 90,
    },
    BassFeel {
        density_bias: 350,
        root_weight: 2700,
        fifth_weight: 900,
        approach_chance: 420,
        ghost_chance: 580,
        octave_punch: 220,
        chromatic_spice: 50,
        sustain_bias: -120,
    },
    BassFeel {
        density_bias: -120,
        root_weight: 2400,
        fifth_weight: 1100,
        approach_chance: 800,
        ghost_chance: 320,
        octave_punch: 380,
        chromatic_spice: 250,
        sustain_bias: 300,
    },
    BassFeel {
        density_bias: 280,
        root_weight: 1900,
        fifth_weight: 1700,
        approach_chance: 560,
        ghost_chance: 500,
        octave_punch: 400,
        chromatic_spice: 100,
        sustain_bias: -60,
    },
];

fn bass_feel(follow_scale: bool, scale_set: usize) -> BassFeel {
    if follow_scale {
        FEEL_NEUTRAL
    } else {
        BASS_FEELS[scale_set.min(SCALE_COUNT - 1)]
    }
}

pub static CONFIG: Config<PARAMS> = Config::new(
    "Umbra",
    "Bass shadow companion to Contura - root, fifth, and approach lines over shared scale sets",
    Color::Violet,
    AppIcon::Note,
)
.add_param(Param::MidiChannel {
    name: "MIDI Channel",
})
.add_param(Param::MidiNote { name: "Base Note" })
.add_param(Param::Color {
    name: "Color",
    variants: &[
        Color::Violet,
        Color::Cyan,
        Color::Rose,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Pink,
        Color::Orange,
    ],
})
.add_param(Param::MidiOut)
.add_param(Param::Range {
    name: "Range",
    variants: &[Range::_0_10V, Range::_0_5V, Range::_Neg5_5V],
})
.add_param(Param::VoltPerOct)
.add_param(Param::bool {
    name: "Follow device tonic",
})
.add_param(Param::bool {
    name: "Follow device scale",
})
.add_param(Param::Enum {
    name: "Scale set",
    variants: SCALE_LABELS,
})
.add_param(Param::Enum {
    name: "Division",
    variants: DIV_LABELS,
})
.add_param(Param::Enum {
    name: "Jack",
    variants: &[
        "CV Out",
        "CV In Density",
        "CV In Interval",
        "CV In Reset",
        "CV In Phrase",
    ],
})
.add_param(Param::i32 {
    name: "CV Att",
    min: 0,
    max: 400,
});

pub struct Params {
    midi_channel: MidiChannel,
    base_note: MidiNote,
    color: Color,
    midi_out: MidiOut,
    range: Range,
    vpo: VoltPerOct,
    follow_tonic: bool,
    follow_scale: bool,
    scale_set: usize,
    division: usize,
    jack: usize,
    cv_att: i32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            midi_channel: MidiChannel::default(),
            base_note: MidiNote::from(36),
            color: Color::Violet,
            midi_out: MidiOut([true, false, false]),
            range: Range::_0_10V,
            vpo: VoltPerOct::Standard,
            follow_tonic: true,
            follow_scale: false,
            scale_set: 0,
            division: 6,
            jack: JACK_OUT,
            cv_att: 100,
        }
    }
}

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.len() < 10 {
            return None;
        }
        let (jack, cv_att) = if values.len() >= 13 {
            let out = usize::from_value(values[10]).min(1) == 0;
            let dest = usize::from_value(values[11]).min(2);
            (
                if out {
                    JACK_OUT
                } else {
                    JACK_IN_DENSITY + dest
                },
                i32::from_value(values[12]).clamp(0, 400),
            )
        } else if values.len() >= PARAMS {
            (
                usize::from_value(values[10]).min(JACK_COUNT - 1),
                i32::from_value(values[11]).clamp(0, 400),
            )
        } else {
            (JACK_OUT, 100)
        };
        Some(Self {
            midi_channel: MidiChannel::from_value(values[0]),
            base_note: MidiNote::from_value(values[1]),
            color: Color::from_value(values[2]),
            midi_out: MidiOut::from_value(values[3]),
            range: Range::from_value(values[4]),
            vpo: VoltPerOct::from_value(values[5]),
            follow_tonic: bool::from_value(values[6]),
            follow_scale: bool::from_value(values[7]),
            scale_set: usize::from_value(values[8]).min(SCALE_COUNT - 1),
            division: usize::from_value(values[9]).min(RESOLUTION.len() - 1),
            jack,
            cv_att,
        })
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        let mut vec = Vec::new();
        vec.push(self.midi_channel.into()).unwrap();
        vec.push(self.base_note.into()).unwrap();
        vec.push(self.color.into()).unwrap();
        vec.push(self.midi_out.into()).unwrap();
        vec.push(self.range.into()).unwrap();
        vec.push(self.vpo.into()).unwrap();
        vec.push(self.follow_tonic.into()).unwrap();
        vec.push(self.follow_scale.into()).unwrap();
        vec.push(self.scale_set.into()).unwrap();
        vec.push(self.division.into()).unwrap();
        vec.push(self.jack.into()).unwrap();
        vec.push(self.cv_att.into()).unwrap();
        vec
    }
}

fn att_from_pct(pct: i32) -> u16 {
    ((pct.clamp(0, 400) as u32 * 4095) / 100) as u16
}

fn mod_u16(base: u16, in_val: u16) -> u16 {
    (base as i32 + in_val as i32 - 2047).clamp(0, 4095) as u16
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Storage {
    interval_saved: u16,
    phrase_saved: u16,
    density_saved: u16,
    scale_set: u8,
    octaves: u8,
    muted: bool,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            interval_saved: 2048,
            phrase_saved: 2048,
            density_saved: 1800,
            scale_set: 0,
            octaves: 1,
            muted: false,
        }
    }
}

impl AppStorage for Storage {}

/// Umbra register: one or two octaves above base only.
fn clamp_octaves(o: u8) -> u8 {
    o.clamp(1, 2)
}

fn cycle_octaves(o: u8) -> u8 {
    if clamp_octaves(o) >= 2 {
        1
    } else {
        2
    }
}

fn set_color(idx: u8) -> Color {
    SET_COLORS[idx as usize % SET_COLORS.len()]
}

fn midi_u8(note: MidiNote) -> u8 {
    u7::from(note).as_int()
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

fn phrase_from_fader(v: u16) -> u8 {
    let span = (MAX_PHRASE - MIN_PHRASE) as u32;
    (MIN_PHRASE as u32 + (v as u32 * span) / 4095) as u8
}

fn mild_curve_u12(v: u16) -> u32 {
    let t = v as u32;
    (t * t + t * 4095) / (2 * 4095)
}

fn density_from_fader(v: u16) -> u16 {
    let curved = mild_curve_u12(v);
    (500 + (curved * 3550) / 4095) as u16
}

/// Main fader → semitone wander budget from root anchor.
fn wander_budget_from_fader(v: u16) -> u8 {
    let curved = mild_curve_u12(v);
    (2 + (curved * 22) / 4095) as u8
}

fn pick_rest(roll: u16, density: u16, steps_left: u8) -> u8 {
    let over = u32::from(roll.saturating_sub(density));
    let span = u32::from(4095u16.saturating_sub(density)).max(1);
    let n = if over * 4 > span * 3 {
        4
    } else if over * 2 > span {
        2
    } else {
        1
    };
    n.min(steps_left.max(1))
}

fn note_velocity(roll: u16, phrase_step: u8, dur: u8, feel: BassFeel, ghost: bool) -> u16 {
    let mut v: i32 = if ghost { 1400 } else { 2800 };
    if phrase_step == 0 && !ghost {
        v += 600;
    }
    if dur >= 3 && !ghost {
        v += 200;
    } else if dur <= 1 && !ghost {
        v -= 200;
    }
    v += i32::from(feel.sustain_bias) * 2;
    v += i32::from((roll >> 4) % 256) - 128;
    v.clamp(if ghost { 700 } else { 1200 }, if ghost { 2200 } else { 4000 }) as u16
}

fn min_duration_for_div(div: u32) -> u8 {
    match div {
        0..=2 => 4,
        3..=4 => 3,
        5..=8 => 2,
        _ => 1,
    }
}

fn midi_gap_ms_for_div(div: u32) -> u16 {
    match div {
        0..=4 => 24,
        5..=8 => 20,
        9..=16 => 18,
        _ => 16,
    }
}

fn pick_duration(die: &Die, wander: u16, remain: u8, feel: BassFeel, min_dur: u8) -> u8 {
    let roll = die.roll();
    let long_bias = (wander as i32 + feel.sustain_bias as i32).clamp(0, 4095) as u16;
    let short_gate = 1200u32 + (4095u32 - long_bias as u32) / 2;
    let mid_gate = 2800u32.saturating_add_signed(feel.sustain_bias as i32 / 2);
    let dur = if (roll as u32) < short_gate {
        min_dur
    } else if (roll as u32) < mid_gate {
        min_dur + 1 + ((roll % 3) as u8)
    } else {
        (remain / 2).max(min_dur + 1)
    };
    let hi = remain.max(1);
    let lo = min_dur.max(1).min(hi);
    dur.clamp(lo, hi)
}

/// Hard fold into Base ± 1 octave.
fn fold_note(note: u8, base: u8) -> u8 {
    let lo = base.saturating_sub(12);
    let hi = (base as u16 + 12).min(127) as u8;
    if note >= lo && note <= hi {
        return note;
    }
    let pc = note % 12;
    let mut best = base.clamp(lo, hi);
    let mut best_dist = u8::MAX;
    let mut anchor = lo;
    while anchor <= hi {
        let candidate = anchor / 12 * 12 + pc;
        let candidate = if candidate < lo {
            candidate + 12
        } else if candidate > hi {
            candidate.saturating_sub(12)
        } else {
            candidate
        };
        if candidate >= lo && candidate <= hi {
            let dist = candidate.abs_diff(base);
            if dist < best_dist {
                best_dist = dist;
                best = candidate;
            }
        }
        anchor = anchor.saturating_add(12);
        if anchor > hi && anchor > 127 {
            break;
        }
    }
    best
}

fn nearest_pc_index(pool: &[u8], pc: u8, near: u8) -> usize {
    if pool.is_empty() {
        return 0;
    }
    let pc = pc % 12;
    let mut best = 0usize;
    let mut best_dist = u8::MAX;
    for (i, &n) in pool.iter().enumerate() {
        if n % 12 != pc {
            continue;
        }
        let dist = n.abs_diff(near);
        if dist < best_dist {
            best_dist = dist;
            best = i;
        }
    }
    best
}

fn fifth_pc(root_pc: u8) -> u8 {
    (root_pc + 7) % 12
}

fn approach_note(
    pool: &[u8],
    target: u8,
    root_pc: u8,
    feel: BassFeel,
    roll: u16,
    chromatic: bool,
) -> Option<u8> {
    if pool.is_empty() {
        return None;
    }
    let target_idx = pool.iter().position(|&n| n == target).unwrap_or(0);
    if chromatic && (roll & 0xfff) < feel.chromatic_spice {
        let below = target.saturating_sub(1);
        return Some(fold_note(below, pool[0]));
    }
    if target_idx > 0 {
        return Some(pool[target_idx - 1]);
    }
    let above_pc = (root_pc + 1) % 12;
    let idx = nearest_pc_index(pool, above_pc, target);
    pool.get(idx).copied()
}

fn apply_octave_punch(note: u8, base: u8, feel: BassFeel, roll: u16) -> u8 {
    if (roll & 0xfff) >= feel.octave_punch {
        return note;
    }
    let up = note.saturating_add(12);
    let down = note.saturating_sub(12);
    let lo = base.saturating_sub(12);
    let hi = (base as u16 + 12).min(127) as u8;
    if up <= hi && (roll & 1) == 0 {
        up
    } else if down >= lo {
        down
    } else {
        note
    }
}

fn pick_bass_target(
    pool: &[u8],
    feel: BassFeel,
    wander: u8,
    root_pc: u8,
    base: u8,
    roll: u16,
) -> u8 {
    if pool.is_empty() {
        return fold_note(base, base);
    }
    let root_idx = nearest_pc_index(pool, root_pc, base);
    let fifth_idx = nearest_pc_index(pool, fifth_pc(root_pc), pool[root_idx]);
    let total = feel
        .root_weight
        .saturating_add(feel.fifth_weight)
        .saturating_add(u16::from(wander) * 40)
        .max(1);
    let role = roll % total;
    let raw = if role < feel.root_weight {
        pool[root_idx]
    } else if role < feel.root_weight.saturating_add(feel.fifth_weight) {
        pool[fifth_idx.min(pool.len() - 1)]
    } else {
        let max_step = (wander as usize).min(pool.len().saturating_sub(1)).max(1);
        let step = 1 + ((roll as usize >> 4) % max_step);
        let signed = if (roll & 1) == 0 {
            root_idx as isize + step as isize
        } else {
            root_idx as isize - step as isize
        };
        let idx = signed.clamp(0, pool.len() as isize - 1) as usize;
        pool[idx]
    };
    fold_note(raw, base)
}

fn ghost_chance_at_density(density_f: u16, feel: BassFeel) -> u16 {
    if density_f < 2000 {
        return 0;
    }
    let t = ((density_f - 2000) as u32 * feel.ghost_chance as u32) / 2095;
    t.min(2800) as u16
}

#[embassy_executor::task(pool_size = 4)]
pub async fn wrapper(app: App<CHANNELS>, exit_signal: &'static Signal<NoopRawMutex, bool>) {
    let param_store = ParamStore::<Params>::new(app.app_id, app.layout_id, Params::default());
    let storage = ManagedStorage::<Storage>::new(app.app_id, app.layout_id);

    param_store.load().await;
    storage.load().await;
    let scale_init = clamp_scale(param_store.query(|p| p.scale_set as u8));
    storage.modify(|s| s.scale_set = scale_init);

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
        base_note,
        led_color,
        range,
        vpo,
        follow_tonic,
        follow_scale,
        scale_param,
        division,
        jack_param,
        cv_att,
    ) = params.query(|p| {
        (
            p.midi_out,
            p.midi_channel,
            p.base_note,
            p.color,
            p.range,
            p.vpo,
            p.follow_tonic,
            p.follow_scale,
            p.scale_set,
            p.division,
            p.jack.min(JACK_COUNT - 1),
            att_from_pct(p.cv_att),
        )
    });

    let buttons = app.use_buttons();
    let faders = app.use_faders();
    let leds = app.use_leds();
    let mut clock = app.use_clock();
    let glob_ticks = app.make_global(0u64);
    let die = app.use_die();
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

    let (interval0, phrase0, density0, _scale0, octaves0, muted0) = storage.query(|s| {
        (
            s.interval_saved,
            s.phrase_saved,
            s.density_saved,
            s.scale_set,
            s.octaves,
            s.muted,
        )
    });

    let scale_init = clamp_scale(scale_param as u8);
    let base_u8 = midi_u8(base_note);

    let glob_interval = app.make_global(interval0);
    let glob_phrase = app.make_global(phrase0);
    let glob_density = app.make_global(density0);
    let glob_div = app.make_global(RESOLUTION[division.min(RESOLUTION.len() - 1)]);
    let glob_scale = app.make_global(scale_init);
    let glob_octaves = app.make_global(clamp_octaves(octaves0));
    let glob_muted = app.make_global(muted0);
    let glob_latch = app.make_global(LatchLayer::Main);
    let glob_fader_moved = app.make_global(false);
    let glob_octave_blink = app.make_global(0u16);
    let glob_button_duck = app.make_global(0u16);
    let glob_pitch_led = app.make_global(2047u16);
    let long_press_fired = app.make_global(false);
    let glob_shift_chord = app.make_global(false);
    let glob_resets_voice = app.make_global(false);
    let glob_scale_dirty = app.make_global(false);
    let glob_fader_dirty = app.make_global(false);
    let glob_cv_val = app.make_global(2047u16);
    let glob_reset = app.make_global(false);

    if muted0 {
        leds.unset(0, Led::Button);
    } else {
        leds.set(0, Led::Button, set_color(scale_init), LED_BRIGHTNESS);
    }

    let glob_silence_req = app.make_global(false);
    let pending_fire = app.make_global(false);
    let pending_note = app.make_global(0u8);
    let pending_vel = app.make_global(3200u16);
    let pending_note_off = app.make_global(false);
    let pending_silence = app.make_global(false);
    let glob_gate_on = app.make_global(false);
    let glob_midi_div = app.make_global(RESOLUTION[division.min(RESOLUTION.len() - 1)]);

    let fut_engine = async {
        let mut pool: Vec<u8, POOL_CAP> = Vec::new();
        let mut phrase_step: u8 = 0;
        let mut remain: u8 = 0;
        let mut gated = false;
        let mut cached_tonic = 0u8;
        let mut pending_main: Option<(u8, u16, u8)> = None;

        let rebuild = |pool: &mut Vec<u8, POOL_CAP>,
                       cached_tonic: &mut u8,
                       scale_set: u8,
                       octaves: u8,
                       base: u8|
         -> usize {
            let (mask, tonic) =
                follow_mask_tonic(follow_scale, follow_tonic, scale_set as usize, base_note);
            *cached_tonic = tonic;
            *pool = build_pool(mask, tonic, base, octaves);
            for n in pool.iter_mut() {
                *n = fold_note(*n, base);
            }
            pool.len()
        };

        let mut last_scale = glob_scale.get();
        let mut last_oct = glob_octaves.get();
        let plen0 = rebuild(
            &mut pool,
            &mut cached_tonic,
            last_scale,
            last_oct,
            base_u8,
        );
        let mut last_note = pool.get(plen0 / 3).copied().unwrap_or(base_u8);

        let mut last_seen = glob_ticks.get();
        let mut last_div_fire: u64 = u64::MAX;
        let mut stall_ms = 0u16;
        let mut prev_gate_high = false;

        let silence = |gated: &mut bool,
                       remain: &mut u8,
                       phrase_step: &mut u8,
                       pending_main: &mut Option<(u8, u16, u8)>| {
            pending_fire.set(false);
            pending_note_off.set(false);
            pending_silence.set(true);
            if let Some(ref jack) = out_jack {
                jack.set_value(0);
            }
            *gated = false;
            *remain = 0;
            *phrase_step = 0;
            *pending_main = None;
            glob_gate_on.set(false);
        };

        let fire_note = |note: u8, vel: u16, gated: &mut bool, plen: usize, note_idx: usize| {
            if let Some(ref jack) = out_jack {
                jack.set_value(note_to_pitch(note).as_counts(range, vpo));
            }
            pending_note.set(note);
            pending_vel.set(vel);
            pending_fire.set(true);
            *gated = true;
            glob_gate_on.set(true);
            glob_button_duck.set(BUTTON_DUCK_MS);
            let pf = if plen > 1 {
                (note_idx as u16 * 4095) / (plen as u16 - 1)
            } else {
                2047
            };
            glob_pitch_led.set(pf);
        };

        loop {
            app.delay_millis(2).await;

            if let Some(ref input) = in_jack {
                let in_val = attenuate_bipolar(input.get_value(), cv_att);
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

            if glob_silence_req.get() {
                glob_silence_req.set(false);
                silence(
                    &mut gated,
                    &mut remain,
                    &mut phrase_step,
                    &mut pending_main,
                );
            }

            let t = glob_ticks.get();
            if t == last_seen {
                stall_ms = stall_ms.saturating_add(2);
                if stall_ms >= 250 && gated {
                    silence(
                        &mut gated,
                        &mut remain,
                        &mut phrase_step,
                        &mut pending_main,
                    );
                }
                continue;
            }
            stall_ms = 0;

            if t < last_seen {
                silence(
                    &mut gated,
                    &mut remain,
                    &mut phrase_step,
                    &mut pending_main,
                );
                last_seen = t;
                last_div_fire = u64::MAX;
                continue;
            }

            let div = glob_div.get().max(1) as u64;
            glob_midi_div.set(div as u32);
            let boundary = t - (t % div);
            last_seen = t;

            let phrase_f = if jack_param == JACK_IN_PHRASE {
                mod_u16(glob_phrase.get(), glob_cv_val.get())
            } else {
                glob_phrase.get()
            };
            let muted = glob_muted.get();
            let scale_set = glob_scale.get();
            let octaves = glob_octaves.get();
            let scale_change =
                scale_set != last_scale || octaves != last_oct || glob_resets_voice.get();

            if glob_reset.get() {
                glob_reset.set(false);
                phrase_step = 0;
                remain = 0;
                pending_main = None;
                pending_fire.set(false);
                if gated {
                    pending_note_off.set(true);
                    gated = false;
                    glob_gate_on.set(false);
                }
            } else if scale_change {
                glob_resets_voice.set(false);
                pending_main = None;
                pending_fire.set(false);
                if gated {
                    pending_note_off.set(true);
                    gated = false;
                    glob_gate_on.set(false);
                }
            } else if muted {
                pending_main = None;
                pending_fire.set(false);
                if gated {
                    pending_note_off.set(true);
                    gated = false;
                    glob_gate_on.set(false);
                }
            }

            if boundary == 0 && t < div {
                continue;
            }
            if boundary == last_div_fire {
                continue;
            }
            last_div_fire = boundary;

            let interval = if jack_param == JACK_IN_INTERVAL {
                mod_u16(glob_interval.get(), glob_cv_val.get())
            } else {
                glob_interval.get()
            };
            let density_f = if jack_param == JACK_IN_DENSITY {
                mod_u16(glob_density.get(), glob_cv_val.get())
            } else {
                glob_density.get()
            };

            if scale_change {
                remain = 0;
                let plen = rebuild(
                    &mut pool,
                    &mut cached_tonic,
                    scale_set,
                    octaves,
                    base_u8,
                );
                last_scale = scale_set;
                last_oct = octaves;
                last_note = pool.get(plen / 3).copied().unwrap_or(base_u8);
                phrase_step = 0;
            }
            let plen = pool.len();
            if plen == 0 {
                continue;
            }
            let feel = bass_feel(follow_scale, scale_set as usize);
            let phrase_len = phrase_from_fader(phrase_f).max(1);
            let density = (density_from_fader(density_f) as i32 + feel.density_bias as i32)
                .clamp(200, 4090) as u16;
            let wander = wander_budget_from_fader(interval);

            if muted {
                remain = 0;
                glob_gate_on.set(false);
                continue;
            }

            if remain > 0 {
                remain -= 1;
                if remain == 0 {
                    if let Some((main_note, main_vel, main_dur)) = pending_main.take() {
                        let note_idx = pool.iter().position(|&n| n == main_note).unwrap_or(0);
                        fire_note(main_note, main_vel, &mut gated, plen, note_idx);
                        remain = main_dur;
                        last_note = main_note;
                    } else if gated {
                        pending_note_off.set(true);
                        gated = false;
                        glob_gate_on.set(false);
                    }
                }
            } else {
                let r = die.roll();
                let steps_left = phrase_len.saturating_sub(phrase_step).max(1);
                let ghost_gate = ghost_chance_at_density(density_f, feel);

                if r > density {
                    if ghost_gate > 0 && (r >> 2) < ghost_gate {
                        let ghost_idx = nearest_pc_index(
                            pool.as_slice(),
                            cached_tonic,
                            last_note,
                        );
                        let ghost_note = pool[ghost_idx.min(plen - 1)];
                        if gated {
                            pending_note_off.set(true);
                        }
                        let gvel = note_velocity(r, phrase_step, 1, feel, true);
                        fire_note(ghost_note, gvel, &mut gated, plen, ghost_idx);
                        remain = 1;
                        last_note = ghost_note;
                    } else {
                        if gated {
                            pending_note_off.set(true);
                            gated = false;
                            glob_gate_on.set(false);
                        }
                        remain = pick_rest(r, density, steps_left);
                    }
                } else {
                    let tonic_pc = cached_tonic;
                    let mut target =
                        pick_bass_target(pool.as_slice(), feel, wander, tonic_pc, base_u8, r);
                    target = apply_octave_punch(target, base_u8, feel, r);
                    let note_idx = pool.iter().position(|&n| n == target).unwrap_or(0);

                    remain = pick_duration(
                        &die,
                        interval,
                        steps_left,
                        feel,
                        min_duration_for_div(div as u32),
                    )
                    .max(1);
                    let vel = note_velocity(r, phrase_step, remain, feel, false);

                    let use_approach = (r >> 3) < feel.approach_chance && div >= 4;
                    if use_approach {
                        if let Some(app) = approach_note(
                            pool.as_slice(),
                            target,
                            tonic_pc,
                            feel,
                            r,
                            true,
                        ) {
                            if gated {
                                pending_note_off.set(true);
                            }
                            let app_vel =
                                note_velocity(r.wrapping_add(17), phrase_step, 1, feel, true);
                            let app_idx = pool.iter().position(|&n| n == app).unwrap_or(0);
                            fire_note(app, app_vel, &mut gated, plen, app_idx);
                            pending_main = Some((target, vel, remain));
                            remain = 1;
                            last_note = app;
                        } else if gated {
                            pending_note_off.set(true);
                            fire_note(target, vel, &mut gated, plen, note_idx);
                            last_note = target;
                        } else {
                            fire_note(target, vel, &mut gated, plen, note_idx);
                            last_note = target;
                        }
                    } else {
                        if gated {
                            pending_note_off.set(true);
                        }
                        fire_note(target, vel, &mut gated, plen, note_idx);
                        last_note = target;
                    }
                }
            }

            phrase_step = phrase_step.wrapping_add(1);
            if phrase_step >= phrase_len {
                phrase_step = 0;
                let (_, tonic) =
                    follow_mask_tonic(follow_scale, follow_tonic, scale_set as usize, base_note);
                if tonic != cached_tonic {
                    let plen = rebuild(
                        &mut pool,
                        &mut cached_tonic,
                        scale_set,
                        octaves,
                        base_u8,
                    );
                    last_note = pool
                        .get(plen.saturating_sub(1) / 3)
                        .copied()
                        .unwrap_or(base_u8);
                }
            }
        }
    };

    let fut_voice = async {
        let mut note_on: Option<u8> = None;
        let mut midi_quiet_ms: u16 = 0;
        loop {
            app.delay_millis(1).await;
            midi_quiet_ms = midi_quiet_ms.saturating_sub(1);

            if pending_silence.get() {
                pending_silence.set(false);
                pending_fire.set(false);
                pending_note_off.set(false);
                if let Some(n) = note_on.take() {
                    midi.send_note_off(MidiNote::from(n)).await;
                }
                continue;
            }

            if pending_note_off.get() {
                pending_note_off.set(false);
                if let Some(n) = note_on.take() {
                    midi.send_note_off(MidiNote::from(n)).await;
                }
            }

            if pending_fire.get() {
                if glob_muted.get() {
                    pending_fire.set(false);
                    continue;
                }
                let note = pending_note.get();
                let pitch_changed = note_on != Some(note);
                let gap = midi_gap_ms_for_div(glob_midi_div.get());
                if !pitch_changed && midi_quiet_ms > 0 {
                    pending_fire.set(false);
                    continue;
                }
                if let Some(old) = note_on {
                    if old != note {
                        midi.send_note_off(MidiNote::from(old)).await;
                    }
                }
                midi.send_note_on(MidiNote::from(note), pending_vel.get())
                    .await;
                pending_fire.set(false);
                note_on = Some(note);
                midi_quiet_ms = gap;
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
                LatchLayer::Main => glob_interval.get(),
                LatchLayer::Alt => glob_phrase.get(),
                LatchLayer::Third => glob_density.get(),
            };

            if let Some(v) = latch.update(faders.get_value(), layer, target) {
                match layer {
                    LatchLayer::Main => {
                        glob_interval.set(v);
                    }
                    LatchLayer::Alt => {
                        glob_phrase.set(v);
                    }
                    LatchLayer::Third => {
                        glob_density.set(v);
                    }
                }
                glob_fader_dirty.set(true);
            }
        }
    };

    let fut_buttons = async {
        loop {
            let (_, down_shift) = buttons.wait_for_any_down().await;
            let shift_chord = down_shift || buttons.is_shift_pressed();
            glob_shift_chord.set(shift_chord);
            long_press_fired.set(false);
            glob_fader_moved.set(false);
            buttons.wait_for_up(0).await;
            glob_shift_chord.set(false);

            if long_press_fired.get() {
                continue;
            }

            if shift_chord {
                let oct = cycle_octaves(glob_octaves.get());
                glob_octaves.set(oct);
                glob_resets_voice.set(true);
                glob_fader_dirty.set(true);
                leds.set(
                    0,
                    Led::Top,
                    OCT_COLORS[(oct - 1) as usize],
                    Brightness::High,
                );
                glob_octave_blink.set(OCTAVE_BLINK_MS);
            } else if !glob_fader_moved.get() {
                let muted = glob_muted.toggle();
                glob_fader_dirty.set(true);
                if muted {
                    glob_silence_req.set(true);
                    leds.unset(0, Led::Button);
                    leds.unset(0, Led::Top);
                    leds.unset(0, Led::Bottom);
                } else {
                    leds.set(0, Led::Button, set_color(glob_scale.get()), LED_BRIGHTNESS);
                }
            }
        }
    };

    let fut_long = async {
        loop {
            let (_, is_shift_now) = buttons.wait_for_any_long_press().await;
            long_press_fired.set(true);
            let shift_chord = glob_shift_chord.get() || is_shift_now || buttons.is_shift_pressed();

            if shift_chord {
                let prev = prev_scale(glob_scale.get());
                glob_scale.set(prev);
                glob_resets_voice.set(true);
                glob_scale_dirty.set(true);
                leds.set(0, Led::Button, set_color(prev), Brightness::High);
            } else if !glob_fader_moved.get() {
                let next = next_scale(glob_scale.get());
                glob_scale.set(next);
                glob_resets_voice.set(true);
                glob_scale_dirty.set(true);
                leds.set(0, Led::Button, set_color(next), Brightness::High);
            }
        }
    };

    let fut_scale_persist = async {
        loop {
            app.delay_millis(400).await;
            let scale_dirty = glob_scale_dirty.get();
            let fader_dirty = glob_fader_dirty.get();
            if !scale_dirty && !fader_dirty {
                continue;
            }
            glob_scale_dirty.set(false);
            glob_fader_dirty.set(false);
            storage.modify_and_save(|st| {
                st.interval_saved = glob_interval.get();
                st.phrase_saved = glob_phrase.get();
                st.density_saved = glob_density.get();
                st.scale_set = clamp_scale(glob_scale.get());
                st.octaves = clamp_octaves(glob_octaves.get());
                st.muted = glob_muted.get();
            });
        }
    };

    let fut_leds = async {
        loop {
            app.delay_millis(8).await;

            let layer = if buttons.is_shift_pressed() && !buttons.is_button_pressed(0) {
                LatchLayer::Alt
            } else if !buttons.is_shift_pressed() && buttons.is_button_pressed(0) {
                LatchLayer::Third
            } else {
                LatchLayer::Main
            };
            glob_latch.set(layer);

            let oct_blink = if glob_octave_blink.get() > 0 {
                let left = glob_octave_blink.get().saturating_sub(8);
                glob_octave_blink.set(left);
                if left == 0 {
                    leds.unset(0, Led::Top);
                }
                left > 0
            } else {
                false
            };

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
            let scale_col = set_color(glob_scale.get());
            let gate = glob_gate_on.get();
            let pitch_m = split_unsigned_value(glob_pitch_led.get());

            match layer {
                LatchLayer::Alt => {
                    let val = glob_phrase.get();
                    let m = split_unsigned_value(val);
                    leds.set(0, Led::Top, Color::White, Brightness::Custom(m[0]));
                    leds.set(0, Led::Bottom, Color::White, Brightness::Custom(m[1]));
                    if !muted {
                        let bright = if duck_active {
                            Brightness::Off
                        } else {
                            signal_brightness(val, false)
                        };
                        leds.set(0, Led::Button, Color::White, bright);
                    }
                }
                LatchLayer::Third => {
                    let val = glob_density.get();
                    let m = split_unsigned_value(val);
                    leds.set(0, Led::Top, scale_col, Brightness::Custom(m[0]));
                    leds.set(0, Led::Bottom, scale_col, Brightness::Custom(m[1]));
                    if !muted {
                        let bright = if duck_active {
                            Brightness::Off
                        } else {
                            signal_brightness(val, false)
                        };
                        leds.set(0, Led::Button, scale_col, bright);
                    }
                }
                LatchLayer::Main => {
                    if gate {
                        if !oct_blink {
                            leds.set(0, Led::Top, led_color, Brightness::Custom(pitch_m[0]));
                        }
                        leds.set(0, Led::Bottom, led_color, Brightness::Custom(pitch_m[1]));
                    } else {
                        if !oct_blink {
                            leds.unset(0, Led::Top);
                        }
                        leds.unset(0, Led::Bottom);
                    }
                    if !muted {
                        let bright = if duck_active {
                            Brightness::Off
                        } else {
                            signal_brightness(glob_interval.get(), false)
                        };
                        leds.set(0, Led::Button, scale_col, bright);
                    }
                }
            }
        }
    };

    let fut_scene = async {
        loop {
            match app.wait_for_scene_event().await {
                SceneEvent::LoadScene(_) => {
                    let (i, p, e, s, o, m) = storage.query(|st| {
                        (
                            st.interval_saved,
                            st.phrase_saved,
                            st.density_saved,
                            st.scale_set,
                            st.octaves,
                            st.muted,
                        )
                    });
                    glob_interval.set(i);
                    glob_phrase.set(p);
                    glob_density.set(e);
                    glob_scale.set(clamp_scale(s));
                    glob_octaves.set(clamp_octaves(o));
                    glob_muted.set(m);
                    glob_resets_voice.set(true);
                    let div = params.query(|p| p.division);
                    glob_div.set(RESOLUTION[div.min(RESOLUTION.len() - 1)]);
                }
                SceneEvent::SaveScene(_) => {}
            }
        }
    };

    let clock_drain = async {
        loop {
            if let ClockEvent::Tick(tick) = clock.wait_for_event(ClockDivision::_1).await {
                glob_ticks.set(tick);
            }
        }
    };

    join5(
        fut_engine,
        fut_voice,
        fut_faders,
        join3(fut_buttons, fut_long, fut_scale_persist),
        join3(fut_leds, fut_scene, clock_drain),
    )
    .await;
}

mod contura_scales {
    use heapless::Vec;
    use libfp::MidiNote;


    mod follow_key {
        use libfp::{Key, MidiNote};
        use midly::num::u7;

        use crate::tasks::global_config::get_global_config;

        pub fn device_key() -> Key {
            match get_global_config().quantizer.key {
                Key::Off => Key::Chromatic,
                k => k,
            }
        }

        pub fn tonic_pc(follow: bool, local_root: MidiNote) -> u8 {
            if follow {
                get_global_config().quantizer.tonic as u8
            } else {
                u7::from(local_root).as_int() % 12
            }
        }
    }

    pub const POOL_CAP: usize = 48;

    // Pitch-class masks, MSB = C … LSB = B (same layout as `Key::as_u16_key`).
    const MASK_INSEN: u16 = 0b110010010010;
    const MASK_YO: u16 = 0b101010010100;
    const MASK_HIRAJOSHI: u16 = 0b110010100010;
    const MASK_BHAIRAV: u16 = 0b110101011001;
    const MASK_KAFI: u16 = 0b101101010110;
    const MASK_BHUPALI: u16 = 0b101010010100;
    const MASK_HIJAZ: u16 = 0b110011011010;
    const MASK_BAYATI: u16 = 0b110101011010;
    const MASK_RAST: u16 = 0b101011010110;
    const MASK_GAMELAN: u16 = 0b110100011000;
    const MASK_HUNGARIAN: u16 = 0b101100111001;
    const MASK_FOLK: u16 = 0b110111011010;

    /// Flat list of named 12-TET sets (Western modes and other named collections,
    /// same footing). Labels are conventional interval-pattern names only.
    pub const SCALE_LABELS: &[&str] = &[
        "Ionian",
        "Dorian",
        "Phrygian",
        "Mixolydian",
        "Aeolian",
        "Pent Maj",
        "Pent Min",
        "Blues Min",
        "In Sen",
        "Yo",
        "Hirajoshi",
        "Bhairav",
        "Kafi",
        "Bhupali",
        "Hijaz",
        "Bayati",
        "Rast",
        "Gamelan",
        "Hungarian",
        "Folk",
    ];

    pub const SCALE_MASKS: [u16; 20] = [
        0b101011010101, // Ionian
        0b101101010110, // Dorian
        0b110101011010, // Phrygian
        0b101011010110, // Mixolydian
        0b101101011010, // Aeolian
        0b101010010100, // Pent Maj
        0b100101010010, // Pent Min
        0b100101110010, // Blues Min
        MASK_INSEN,
        MASK_YO,
        MASK_HIRAJOSHI,
        MASK_BHAIRAV,
        MASK_KAFI,
        MASK_BHUPALI,
        MASK_HIJAZ,
        MASK_BAYATI,
        MASK_RAST,
        MASK_GAMELAN,
        MASK_HUNGARIAN,
        MASK_FOLK,
    ];

    pub const SCALE_COUNT: usize = 20;

    /// Scale list wraps in both directions (Ionian ↔ Folk).
    pub fn wrap_scale(i: isize) -> u8 {
        let n = SCALE_COUNT as isize;
        (((i % n) + n) % n) as u8
    }

    pub fn next_scale(cur: u8) -> u8 {
        wrap_scale(cur as isize + 1)
    }

    pub fn prev_scale(cur: u8) -> u8 {
        wrap_scale(cur as isize - 1)
    }

    pub fn clamp_scale(s: u8) -> u8 {
        (s as usize).min(SCALE_COUNT - 1) as u8
    }

    fn degrees_from_mask(mask: u16) -> Vec<u8, 12> {
        let mut out = Vec::new();
        for i in 0..12u8 {
            if (mask >> (11 - i)) & 1 != 0 {
                let _ = out.push(i);
            }
        }
        if out.is_empty() {
            let _ = out.push(0);
        }
        out
    }

    pub fn follow_mask_tonic(
        follow_scale: bool,
        follow_tonic: bool,
        scale_set: usize,
        base: MidiNote,
    ) -> (u16, u8) {
        let local = SCALE_MASKS[scale_set.min(SCALE_COUNT - 1)];
        // Contura's scale sets are its own (Folk, Hexatonic …), so only the
        // followed case can go through a plain Key.
        let mask = if follow_scale {
            follow_key::device_key().as_u16_key()
        } else {
            local
        };
        (mask, follow_key::tonic_pc(follow_tonic, base))
    }

    pub fn build_pool(mask: u16, tonic: u8, base: u8, octaves: u8) -> Vec<u8, POOL_CAP> {
        let degrees = degrees_from_mask(mask);
        let lo = base;
        let hi = (base as u16 + octaves as u16 * 12).min(127) as u8;
        let mut pool = Vec::new();
        for oct in -2i16..=8 {
            for &deg in degrees.iter() {
                let semi = oct * 12 + deg as i16 + tonic as i16;
                if !(0..=127).contains(&semi) {
                    continue;
                }
                let n = semi as u8;
                if n >= lo && n <= hi {
                    let _ = pool.push(n);
                }
            }
        }
        if pool.is_empty() {
            let _ = pool.push(base.clamp(0, 127));
        }
        pool
    }
}
