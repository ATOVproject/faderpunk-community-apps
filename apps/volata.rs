//! Volata — 2ch staccato melody bursts in device key, always newly generated.

use embassy_futures::{
    join::{join, join5},
    select::{select, select3},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use heapless::Vec;
use libfp::{
    APP_MAX_PARAMS, AppIcon, Brightness, ClockDivision, Color, Config, Key, MidiChannel, MidiNote,
    MidiOut, Note, Param, Range, Value, VoltPerOct, ext::FromValue, latch::LatchLayer,
    quantizer::Pitch,
    utils::{attenuate_bipolar, split_unsigned_value},
};
use midly::num::u7;
use serde::{Deserialize, Serialize};

use crate::app::{
    App, AppParams, AppStorage, ClockEvent, Die, Led, ManagedStorage, ParamStore, SceneEvent,
    pitch_as_counts,
};

pub const CHANNELS: usize = 2;
pub const PARAMS: usize = 11;

const SIXTEENTH: u64 = 6;
const BAR: u64 = 96;
const FALLBACK_MS: u64 = 125;
const MAX_BURST: usize = 32;

const CV_DEST_LENGTH: usize = 0;
const CV_DEST_GAP: usize = 1;
const CV_DEST_GATE: usize = 2;

const VOICING_NOTE: u8 = 0;
const VOICING_CHORD: u8 = 1;
const VOICING_VOICINGS: u8 = 2;

const SCALE_LABELS: &[&str] = &[
    "Chromatic",
    "Ionian",
    "Dorian",
    "Phrygian",
    "Lydian",
    "Mixolydian",
    "Aeolian",
    "Locrian",
    "Blues Major",
    "Blues Minor",
    "Pentatonic Major",
    "Pentatonic Minor",
    "Folk",
    "Japanese",
    "Gamelan",
    "Hungarian Minor",
];

const KEY_CYCLE: [Key; 16] = [
    Key::Chromatic,
    Key::Ionian,
    Key::Dorian,
    Key::Phrygian,
    Key::Lydian,
    Key::Mixolydian,
    Key::Aeolian,
    Key::Locrian,
    Key::BluesMaj,
    Key::BluesMin,
    Key::PentatonicMaj,
    Key::PentatonicMin,
    Key::Folk,
    Key::Japanese,
    Key::Gamelan,
    Key::HungarianMin,
];

const DEGREE_WEIGHTS: [(u8, u16); 6] = [
    (0, 2200),
    (5, 2100),
    (1, 2000),
    (4, 2000),
    (3, 1800),
    (2, 1800),
];

const LED_BRIGHTNESS: Brightness = Brightness::Mid;
const BUTTON_DUCK_MS: u16 = 80;

const SET_COLORS: [Color; 8] = [
    Color::Orange,
    Color::Cyan,
    Color::Violet,
    Color::Rose,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Pink,
];

fn set_color(idx: u8) -> Color {
    SET_COLORS[idx as usize % SET_COLORS.len()]
}

fn voicing_color(mode: u8, app_color: Color) -> Color {
    match mode {
        VOICING_CHORD => Color::Cyan,
        VOICING_VOICINGS => Color::Rose,
        _ => app_color,
    }
}

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

pub static CONFIG: Config<PARAMS> = Config::new(
    "Volata",
    "Staccato melody bursts in device key",
    Color::Rose,
    AppIcon::Note,
)
.add_param(Param::MidiChannel {
    name: "MIDI Channel",
})
.add_param(Param::MidiNote { name: "Base Note" })
.add_param(Param::Color {
    name: "Color",
    variants: &[
        Color::Rose,
        Color::Blue,
        Color::Green,
        Color::Orange,
        Color::Cyan,
        Color::Pink,
        Color::Violet,
        Color::Yellow,
    ],
})
.add_param(Param::MidiOut)
.add_param(Param::Range {
    name: "Range",
    variants: &[Range::_0_10V, Range::_Neg5_5V],
})
.add_param(Param::VoltPerOct)
.add_param(Param::bool {
    name: "Follow device tonic",
})
.add_param(Param::bool {
    name: "Follow device scale",
})
.add_param(Param::Enum {
    name: "Scale",
    variants: SCALE_LABELS,
})
.add_param(Param::Enum {
    name: "CV Dest",
    variants: &["Length", "Gap", "Gate"],
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
    scale_param: usize,
    cv_dest: usize,
    cv_att: i32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            midi_channel: MidiChannel::default(),
            base_note: MidiNote::from(60),
            color: Color::Rose,
            midi_out: MidiOut::default(),
            range: Range::_0_10V,
            vpo: VoltPerOct::Standard,
            follow_tonic: true,
            follow_scale: true,
            scale_param: 1,
            cv_dest: CV_DEST_LENGTH,
            cv_att: 100,
        }
    }
}

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.len() < PARAMS {
            return None;
        }
        Some(Self {
            midi_channel: MidiChannel::from_value(values[0]),
            base_note: MidiNote::from_value(values[1]),
            color: Color::from_value(values[2]),
            midi_out: MidiOut::from_value(values[3]),
            range: Range::from_value(values[4]),
            vpo: VoltPerOct::from_value(values[5]),
            follow_tonic: bool::from_value(values[6]),
            follow_scale: bool::from_value(values[7]),
            scale_param: usize::from_value(values[8]).min(SCALE_LABELS.len() - 1),
            cv_dest: usize::from_value(values[9]).min(2),
            cv_att: i32::from_value(values[10]).clamp(0, 400),
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
        vec.push(self.scale_param.into()).unwrap();
        vec.push(self.cv_dest.into()).unwrap();
        vec.push(self.cv_att.into()).unwrap();
        vec
    }
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Storage {
    fader_main: [u16; 2],
    fader_alt: [u16; 2],
    fader_third: [u16; 2],
    muted: bool,
    scale_idx: u8,
    voicing_mode: u8,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            fader_main: [2800, 2048],
            fader_alt: [1024, 2048],
            fader_third: [2800, 3200],
            muted: false,
            scale_idx: 1,
            voicing_mode: VOICING_NOTE,
        }
    }
}

impl AppStorage for Storage {}

fn att_from_pct(pct: i32) -> u16 {
    ((pct.clamp(0, 400) as u32 * 4095) / 100) as u16
}

fn mod_u16(base: u16, in_val: u16) -> u16 {
    (base as i32 + in_val as i32 - 2047).clamp(0, 4095) as u16
}

fn fader_to_length(v: u16) -> u8 {
    ((v as u32 * 31 / 4095) + 1).clamp(1, 32) as u8
}

fn fader_to_gap(v: u16) -> u16 {
    (v as u32 * 96 / 4095).clamp(0, 96) as u16
}

fn fader_to_gate_pct(v: u16) -> u32 {
    ((v as u32 * 99 / 4095) + 1).clamp(1, 100)
}

fn fader_to_velocity(v: u16) -> u16 {
    v.max(1)
}

fn scale_pcs(key: Key) -> Vec<u8, 12> {
    let mask = if key == Key::Off {
        Key::Chromatic.as_u16_key()
    } else {
        key.as_u16_key()
    };
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

fn pc_near(pc: i16, near: i16) -> i16 {
    let pc = pc.rem_euclid(12);
    let mut m = pc + 12 * (near.div_euclid(12));
    if m - near > 6 {
        m -= 12;
    }
    if near - m > 6 {
        m += 12;
    }
    m.clamp(0, 127)
}

fn degree_to_note(degree: i16, tonic_pc: u8, pcs: &[u8], ref_midi: i16) -> u8 {
    let n = pcs.len().max(1) as i16;
    let idx = degree.rem_euclid(n) as usize;
    let octs = degree.div_euclid(n);
    let pc = (tonic_pc as i16 + pcs[idx] as i16).rem_euclid(12);
    (pc_near(pc, ref_midi) + octs * 12).clamp(0, 127) as u8
}

fn midi_center(base: MidiNote, center_fader: u16) -> i16 {
    let base_i = u7::from(base).as_int() as i16;
    (base_i + ((center_fader as i32 - 2048) * 48 / 2048) as i16).clamp(0, 127)
}

fn pick_degree(die: &Die) -> u8 {
    let roll = die.roll() as u32;
    let total: u32 = DEGREE_WEIGHTS.iter().map(|(_, w)| *w as u32).sum();
    let mut acc = 0u32;
    for (deg, w) in DEGREE_WEIGHTS {
        acc += w as u32;
        if roll < acc * 4095 / total {
            return deg;
        }
    }
    0
}

fn triad_degrees(root: u8, n_degrees: usize) -> [i16; 3] {
    let n = n_degrees.max(1) as i16;
    let r = (root as i16).rem_euclid(n);
    [r, (r + 2).rem_euclid(n), (r + 4).rem_euclid(n)]
}

fn voicing_notes(triad: [i16; 3], spread: u8, tonic_pc: u8, pcs: &[u8], ref_midi: i16) -> [u8; 3] {
    let root = degree_to_note(triad[0], tonic_pc, pcs, ref_midi);
    let third = degree_to_note(triad[1], tonic_pc, pcs, ref_midi);
    let fifth = degree_to_note(triad[2], tonic_pc, pcs, ref_midi);
    match spread % 4 {
        0 => [root, third, fifth],
        1 => [third, fifth, root.saturating_add(12).min(127)],
        2 => [
            fifth,
            root.saturating_add(12).min(127),
            third.saturating_add(12).min(127),
        ],
        _ => [root, fifth, third.saturating_add(12).min(127)],
    }
}

fn pick_from_pool(pool: &[u8], die: &Die, avoid: Option<u8>) -> u8 {
    if pool.is_empty() {
        return 60;
    }
    for _ in 0..8 {
        let n = pool[(die.roll() as usize) % pool.len()];
        if avoid != Some(n) {
            return n;
        }
    }
    pool[0]
}

struct Meander {
    dir: i16,
    contour: u8,
    leap_left: u8,
    after_leap: bool,
}

impl Meander {
    fn new(die: &Die, len: usize) -> Self {
        Self {
            dir: if die.roll() < 2048 { 1 } else { -1 },
            contour: (die.roll() % 4) as u8,
            leap_left: (1 + (die.roll() % 3) as u8).min(len.saturating_sub(2) as u8),
            after_leap: false,
        }
    }
}

fn clamp_deg(d: i16, center: i16, span: i16) -> i16 {
    d.clamp(center - span, center + span)
}

fn meander_next(
    m: &mut Meander,
    prev: i16,
    center: i16,
    span: i16,
    die: &Die,
    step: usize,
    len: usize,
) -> i16 {
    if m.after_leap {
        m.after_leap = false;
        return clamp_deg(prev + m.dir, center, span);
    }

    let t = if len <= 1 {
        0
    } else {
        step * 256 / (len - 1).max(1)
    };

    match m.contour {
        0 => m.dir = if t < 128 { 1 } else { -1 },
        1 => m.dir = if die.roll() < 900 { 1 } else { -1 },
        2 => {
            if step.is_multiple_of(2) {
                m.dir = -m.dir;
            }
            if die.roll() < 900 {
                m.dir = -m.dir;
            }
        }
        _ => {
            m.dir = -m.dir;
            if die.roll() < 600 {
                m.dir = -m.dir;
            }
        }
    }

    let can_leap = m.leap_left > 0 && step > 0 && step + 1 < len && die.roll() < 1400;
    let delta = if can_leap {
        m.leap_left -= 1;
        m.after_leap = true;
        let width = 3 + (die.roll() % 3) as i16;
        let leap_dir = if die.roll() < 2048 { 1 } else { -1 };
        m.dir = -leap_dir;
        width * leap_dir
    } else {
        (1 + (die.roll() % 2) as i16) * m.dir
    };
    clamp_deg(prev + delta, center, span)
}

fn mutate_degrees(degs: &mut [i16], start: i16, die: &Die) {
    let roll = die.roll();
    if roll < 1000 {
        for d in degs.iter_mut().skip(1) {
            *d = start - (*d - start);
        }
    } else if roll < 1800 && degs.len() > 3 {
        let n = degs.len();
        degs[1..n - 1].reverse();
    } else if roll < 2600 {
        let shift = if die.roll() < 2048 { 1 } else { -1 };
        for d in degs.iter_mut() {
            *d += shift;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn next_melody_degree(
    meander: &mut Meander,
    prev: i16,
    triad: [i16; 3],
    center: i16,
    span: i16,
    pcs: &[u8],
    tonic_pc: u8,
    die: &Die,
    step: usize,
    len: usize,
    last_midi: Option<u8>,
    ref_midi: i16,
) -> i16 {
    if step + 1 == len {
        return triad[(die.roll() as usize) % 3] + center - triad[0];
    }

    let mut d = meander_next(meander, prev, center, span, die, step, len);
    let midi = degree_to_note(d, tonic_pc, pcs, ref_midi);
    if last_midi == Some(midi) {
        d = clamp_deg(d + meander.dir, center, span);
    }
    d
}

fn build_note_pool(
    triad: [i16; 3],
    center: i16,
    span: i16,
    tonic_pc: u8,
    pcs: &[u8],
    ref_midi: i16,
) -> Vec<u8, 64> {
    let mut pool = Vec::new();
    let lo = center - span;
    let hi = center + span;
    for d in lo..=hi {
        let midi = degree_to_note(d, tonic_pc, pcs, ref_midi);
        if !pool.contains(&midi) {
            let _ = pool.push(midi);
        }
    }
    for &t in &triad {
        let midi = degree_to_note(t, tonic_pc, pcs, ref_midi);
        if !pool.contains(&midi) {
            let _ = pool.push(midi);
        }
        let neighbor_lo = degree_to_note(t - 1, tonic_pc, pcs, ref_midi);
        let neighbor_hi = degree_to_note(t + 1, tonic_pc, pcs, ref_midi);
        if !pool.contains(&neighbor_lo) {
            let _ = pool.push(neighbor_lo);
        }
        if !pool.contains(&neighbor_hi) {
            let _ = pool.push(neighbor_hi);
        }
    }
    if pool.is_empty() {
        let _ = pool.push(degree_to_note(center, tonic_pc, pcs, ref_midi));
    }
    pool
}

#[allow(clippy::too_many_arguments)]
fn generate_burst(
    len: usize,
    voicing: u8,
    key: Key,
    tonic: Note,
    base: MidiNote,
    center_fader: u16,
    range_fader: u16,
    root_degree: u8,
    die: &Die,
    out: &mut [u8; MAX_BURST],
) -> usize {
    let pcs = scale_pcs(key);
    let tonic_pc = tonic as u8;
    let n = pcs.len().max(1);
    let triad = triad_degrees(root_degree, n);
    let ref_midi = midi_center(base, center_fader);
    let center = triad[0];
    let span = ((range_fader as u32 * 31 / 4095) + 1) as i16;
    let pool = build_note_pool(triad, center, span, tonic_pc, &pcs, ref_midi);

    let len = len.clamp(1, MAX_BURST);
    match voicing {
        VOICING_CHORD => {
            let mut order = [0usize, 1, 2];
            for i in (1..3).rev() {
                let j = (die.roll() as usize) % (i + 1);
                order.swap(i, j);
            }
            let oct = if die.roll() < 1400 { n as i16 } else { 0 };
            let pass = die.roll() < 1600;
            let mut k = 0usize;
            for (i, slot) in out.iter_mut().enumerate().take(len) {
                if pass && !i.is_multiple_of(2) {
                    let a = triad[order[k % 3]];
                    let b = triad[order[(k + 1) % 3]];
                    *slot = degree_to_note((a + b) / 2 + oct, tonic_pc, &pcs, ref_midi);
                } else {
                    *slot = degree_to_note(triad[order[k % 3]] + oct, tonic_pc, &pcs, ref_midi);
                    k += 1;
                }
            }
        }
        VOICING_VOICINGS => {
            let spread = (die.roll() % 4) as u8;
            let voiced = voicing_notes(triad, spread, tonic_pc, &pcs, ref_midi);
            let mut order = [0usize, 1, 2];
            for i in (1..3).rev() {
                let j = (die.roll() as usize) % (i + 1);
                order.swap(i, j);
            }
            for (i, slot) in out.iter_mut().enumerate().take(len) {
                *slot = voiced[order[i % 3]];
            }
        }
        _ => {
            let start_choice = (die.roll() as usize) % 5;
            let start = match start_choice {
                0..=2 => triad[start_choice],
                3 => triad[0] + 1,
                _ => triad[0] - 1,
            };
            let mut degs: Vec<i16, MAX_BURST> = Vec::new();
            let mut meander = Meander::new(die, len);
            let mut deg = start;
            let mut last: Option<u8> = None;
            for i in 0..len {
                if i == 0 {
                    deg = start;
                } else {
                    deg = next_melody_degree(
                        &mut meander,
                        deg,
                        triad,
                        center,
                        span,
                        &pcs,
                        tonic_pc,
                        die,
                        i,
                        len,
                        last,
                        ref_midi,
                    );
                }
                let _ = degs.push(deg);
                last = Some(degree_to_note(deg, tonic_pc, &pcs, ref_midi));
            }
            mutate_degrees(&mut degs, start, die);
            if !degs.is_empty() {
                degs[0] = start;
                let end_t = triad[(die.roll() as usize) % 3];
                let last_i = degs.len() - 1;
                degs[last_i] = end_t;
            }
            let mut prev_midi = 0u8;
            for i in 0..len {
                let mut midi = degree_to_note(degs[i], tonic_pc, &pcs, ref_midi);
                if i > 0 && midi == prev_midi {
                    midi = pick_from_pool(&pool, die, Some(prev_midi));
                }
                out[i] = midi;
                prev_midi = midi;
            }
        }
    }
    len
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

fn cycle_scale_idx(idx: u8, reverse: bool) -> u8 {
    let max = KEY_CYCLE.len() as u8;
    if reverse {
        if idx == 0 { max - 1 } else { idx - 1 }
    } else if idx + 1 >= max {
        0
    } else {
        idx + 1
    }
}

fn cycle_voicing(mode: u8, reverse: bool) -> u8 {
    if reverse {
        if mode == 0 {
            VOICING_VOICINGS
        } else {
            mode - 1
        }
    } else if mode >= VOICING_VOICINGS {
        VOICING_NOTE
    } else {
        mode + 1
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_burst(
    quantizer: &crate::app::Quantizer,
    die: &Die,
    storage: &ManagedStorage<Storage>,
    follow_scale: bool,
    follow_tonic: bool,
    base_note: MidiNote,
    length_slots: u8,
    out: &mut [u8; MAX_BURST],
) -> u8 {
    let (device_key, device_tonic) = quantizer.get_scale().await;
    let key = if follow_scale {
        if device_key == Key::Off {
            Key::Chromatic
        } else {
            device_key
        }
    } else {
        KEY_CYCLE[storage
            .query(|s| s.scale_idx as usize)
            .min(KEY_CYCLE.len() - 1)]
    };
    let tonic = if follow_tonic {
        device_tonic
    } else {
        Note::from(u7::from(base_note).as_int() % 12)
    };
    let root = pick_degree(die);
    generate_burst(
        length_slots as usize,
        storage.query(|s| s.voicing_mode),
        key,
        tonic,
        base_note,
        storage.query(|s| s.fader_main[1]),
        storage.query(|s| s.fader_alt[1]),
        root,
        die,
        out,
    ) as u8
}

#[embassy_executor::task(pool_size = 16 / CHANNELS)]
pub async fn wrapper(app: App<CHANNELS>, exit_signal: &'static Signal<NoopRawMutex, bool>) {
    let param_store = ParamStore::<Params>::new(app.app_id, app.layout_id, Params::default());
    let storage = ManagedStorage::<Storage>::new(app.app_id, app.layout_id);

    param_store.load().await;
    storage.load().await;

    let app_loop = async {
        while true {
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
        cv_dest,
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
            p.scale_param,
            p.cv_dest.min(2),
            att_from_pct(p.cv_att),
        )
    });

    let cv_in = app.make_in_jack(0, Range::_Neg5_5V).await;
    let pitch_out = app.make_out_jack(1, range).await;

    let faders = app.use_faders();
    let buttons = app.use_buttons();
    let leds = app.use_leds();
    let mut clk = app.use_clock();
    let die = app.use_die();
    let quantizer = app.use_quantizer(range, vpo, false);
    let midi = app.use_midi_output(midi_out, midi_chan, false);

    let glob_ticks = app.make_global(0u64);
    let glob_running = app.make_global(false);
    let glob_stop = app.make_global(false);
    let glob_reset = app.make_global(false);
    let glob_latch_layer = app.make_global(LatchLayer::Main);
    let glob_muted = app.make_global(storage.query(|s| s.muted));
    let glob_cv_val = app.make_global(2047u16);

    let manual_fire = app.make_global(false);
    let note_on_pending = app.make_global(false);
    let pending_note = app.make_global(0u8);
    let pending_vel = app.make_global(2048u16);
    let pending_note_off = app.make_global(false);
    let pending_silence = app.make_global(false);
    let pending_cv_counts = app.make_global(0u16);

    let long_press_fired = app.make_global(false);
    let fader_moved_during_press = app.make_global(false);
    let glob_button_duck = app.make_global(0u16);
    let glob_gate_on = app.make_global(false);
    let glob_pitch_led = app.make_global(0u16);
    let glob_burst_led = app.make_global(0u16);

    let (main_saved, alt_saved, third_saved, voicing) = storage.query(|s| {
        (
            s.fader_main,
            s.fader_alt,
            s.fader_third,
            s.voicing_mode.min(VOICING_VOICINGS),
        )
    });

    storage.modify(|s| {
        s.scale_idx = (scale_param as u8).min((KEY_CYCLE.len() - 1) as u8);
    });
    let (scale_idx, voicing_mode) = storage.query(|s| (s.scale_idx, s.voicing_mode));

    if glob_muted.get() {
        leds.unset_chan(0);
        leds.unset_chan(1);
    } else {
        leds.set(0, Led::Button, set_color(scale_idx), LED_BRIGHTNESS);
        leds.set(
            1,
            Led::Button,
            voicing_color(voicing_mode, led_color),
            LED_BRIGHTNESS,
        );
    }
    let _ = (main_saved, alt_saved, third_saved, voicing);

    let clock_drain = async {
        while true {
            match clk.wait_for_event(ClockDivision::_1).await {
                ClockEvent::Tick(tick) => {
                    glob_ticks.set(tick);
                    glob_running.set(true);
                    glob_stop.set(false);
                }
                ClockEvent::Stop => {
                    glob_running.set(false);
                    glob_stop.set(true);
                }
                ClockEvent::Reset => {
                    glob_reset.set(true);
                    glob_ticks.set(0);
                }
                _ => {}
            }
        }
    };

    let fut_engine = async {
        let mut burst_notes = [60u8; MAX_BURST];
        let mut burst_len: u8 = 0;
        let mut burst_step: u8 = 0;
        let mut burst_active = false;
        let mut slots_since_burst: u16 = u16::MAX;
        let mut last_sixteenth_tick: u64 = u64::MAX;
        let mut last_bar_tick: u64 = u64::MAX;
        let mut gate_off_tick: u64 = 0;
        let mut gate_open = false;
        let mut free_ms_acc: u64 = 0;
        let mut free_gate_remain: u64 = 0;

        while true {
            app.delay_millis(1).await;

            if glob_muted.get() || glob_stop.get() || glob_reset.get() {
                pending_silence.set(true);
                burst_active = false;
                gate_open = false;
                glob_gate_on.set(false);
                free_gate_remain = 0;
                if glob_reset.get() {
                    glob_reset.set(false);
                    slots_since_burst = u16::MAX;
                }
                if glob_stop.get() {
                    glob_stop.set(false);
                }
            }

            let running = glob_running.get();
            let tick = glob_ticks.get();

            let cv = glob_cv_val.get();
            let length_fader = match cv_dest {
                CV_DEST_LENGTH => mod_u16(storage.query(|s| s.fader_main[0]), cv),
                _ => storage.query(|s| s.fader_main[0]),
            };
            let gap_fader = match cv_dest {
                CV_DEST_GAP => mod_u16(storage.query(|s| s.fader_alt[0]), cv),
                _ => storage.query(|s| s.fader_alt[0]),
            };
            let gate_fader = match cv_dest {
                CV_DEST_GATE => mod_u16(storage.query(|s| s.fader_third[0]), cv),
                _ => storage.query(|s| s.fader_third[0]),
            };

            let length_slots = fader_to_length(length_fader);
            let gap_slots = fader_to_gap(gap_fader);
            let gate_pct = fader_to_gate_pct(gate_fader);
            let velocity = fader_to_velocity(storage.query(|s| s.fader_third[1]));

            let mut start_new_burst = false;
            let mut force_slot = false;
            if manual_fire.get() {
                manual_fire.set(false);
                start_new_burst = true;
                force_slot = true;
            } else if running
                && tick.is_multiple_of(BAR)
                && tick != last_bar_tick
                && !glob_muted.get()
                && (slots_since_burst >= gap_slots || slots_since_burst == u16::MAX)
            {
                last_bar_tick = tick;
                start_new_burst = true;
            } else if running && tick.is_multiple_of(BAR) {
                last_bar_tick = tick;
            }

            if start_new_burst && !glob_muted.get() {
                if gate_open {
                    pending_note_off.set(true);
                    gate_open = false;
                }
                burst_len = prepare_burst(
                    &quantizer,
                    &die,
                    storage,
                    follow_scale,
                    follow_tonic,
                    base_note,
                    length_slots,
                    &mut burst_notes,
                )
                .await;
                burst_step = 0;
                burst_active = true;
                if !running {
                    free_ms_acc = FALLBACK_MS;
                }
            }

            let sixteenth_edge = force_slot
                || if running {
                    tick.is_multiple_of(SIXTEENTH) && tick != last_sixteenth_tick
                } else if burst_active {
                    free_ms_acc = free_ms_acc.saturating_add(1);
                    free_ms_acc >= FALLBACK_MS
                } else {
                    false
                };

            if sixteenth_edge {
                if running {
                    last_sixteenth_tick = tick;
                } else {
                    free_ms_acc = 0;
                }

                if burst_active && burst_step < burst_len {
                    let raw = burst_notes[burst_step as usize];
                    pending_note.set(raw);
                    pending_vel.set(velocity);
                    pending_cv_counts.set(pitch_as_counts(note_to_pitch(raw), range, vpo));
                    note_on_pending.set(true);
                    glob_gate_on.set(true);
                    glob_button_duck.set(BUTTON_DUCK_MS);
                    glob_pitch_led.set(((raw as u32 * 4095) / 127) as u16);
                    let denom = burst_len.max(1) as u32;
                    glob_burst_led.set(((burst_step as u32 * 4095) / denom) as u16);

                    if running {
                        gate_off_tick = tick + (SIXTEENTH * gate_pct as u64 / 100).max(1);
                    } else {
                        free_gate_remain = (FALLBACK_MS * gate_pct as u64 / 100).max(1);
                    }
                    gate_open = true;
                    burst_step += 1;
                }

                if burst_active && burst_step >= burst_len {
                    burst_active = false;
                    slots_since_burst = 0;
                } else if !burst_active {
                    slots_since_burst = slots_since_burst.saturating_add(1);
                }
            }

            if gate_open && running && tick >= gate_off_tick {
                pending_note_off.set(true);
                gate_open = false;
                glob_gate_on.set(false);
            }

            if gate_open && !running {
                if free_gate_remain > 0 {
                    free_gate_remain = free_gate_remain.saturating_sub(1);
                }
                if free_gate_remain == 0 {
                    pending_note_off.set(true);
                    gate_open = false;
                    glob_gate_on.set(false);
                }
            }
        }
    };

    let fut_voice = async {
        let mut note_on: Option<MidiNote> = None;
        while true {
            app.delay_millis(1).await;

            if pending_silence.get() {
                pending_silence.set(false);
                if let Some(n) = note_on.take() {
                    midi.send_note_off(n).await;
                }
                pitch_out.set_value(0);
                continue;
            }

            if pending_note_off.get() {
                pending_note_off.set(false);
                if let Some(n) = note_on.take() {
                    midi.send_note_off(n).await;
                }
            }

            if note_on_pending.get() {
                note_on_pending.set(false);
                if glob_muted.get() {
                    continue;
                }
                let raw = pending_note.get();
                let midi_n = MidiNote::from(raw);
                let vel = pending_vel.get();
                if let Some(prev) = note_on.take() {
                    midi.send_note_off(prev).await;
                }
                midi.send_note_on(midi_n, vel).await;
                note_on = Some(midi_n);
                pitch_out.set_value(pending_cv_counts.get());
            }
        }
    };

    let fader_handler = async {
        let mut latch = [
            app.make_latch(faders.get_value_at(0)),
            app.make_latch(faders.get_value_at(1)),
        ];
        while true {
            let chan = faders.wait_for_any_change().await;
            let layer = glob_latch_layer.get();
            let target = match (chan, layer) {
                (0, LatchLayer::Main) => storage.query(|s| s.fader_main[0]),
                (0, LatchLayer::Alt) => storage.query(|s| s.fader_alt[0]),
                (0, LatchLayer::Third) => storage.query(|s| s.fader_third[0]),
                (1, LatchLayer::Main) => storage.query(|s| s.fader_main[1]),
                (1, LatchLayer::Alt) => storage.query(|s| s.fader_alt[1]),
                (1, LatchLayer::Third) => storage.query(|s| s.fader_third[1]),
                _ => 0,
            };
            if let Some(v) = latch[chan].update(faders.get_value_at(chan), layer, target) {
                fader_moved_during_press.set(true);
                storage.modify_and_save(|s| match (chan, layer) {
                    (0, LatchLayer::Main) => s.fader_main[0] = v,
                    (0, LatchLayer::Alt) => s.fader_alt[0] = v,
                    (0, LatchLayer::Third) => s.fader_third[0] = v,
                    (1, LatchLayer::Main) => s.fader_main[1] = v,
                    (1, LatchLayer::Alt) => s.fader_alt[1] = v,
                    (1, LatchLayer::Third) => s.fader_third[1] = v,
                    _ => {}
                });
            }
        }
    };

    let button_handler = async {
        while true {
            let (chan, shift) = buttons.wait_for_any_down().await;
            long_press_fired.set(false);
            fader_moved_during_press.set(false);

            if chan == 0 && !shift {
                buttons.wait_for_up(0).await;
                if !long_press_fired.get() && !fader_moved_during_press.get() {
                    let muted = storage.modify_and_save(|s| {
                        s.muted = !s.muted;
                        s.muted
                    });
                    glob_muted.set(muted);
                    if muted {
                        pending_silence.set(true);
                        leds.unset_chan(0);
                        leds.unset_chan(1);
                    } else {
                        let (si, vm) = storage.query(|s| (s.scale_idx, s.voicing_mode));
                        leds.set(0, Led::Button, set_color(si), LED_BRIGHTNESS);
                        leds.set(1, Led::Button, voicing_color(vm, led_color), LED_BRIGHTNESS);
                    }
                }
            } else if chan == 1 && !shift {
                buttons.wait_for_up(1).await;
                if !long_press_fired.get() && !fader_moved_during_press.get() {
                    manual_fire.set(true);
                }
            }
        }
    };

    let long_press_handler = async {
        while true {
            let (chan, shift) = buttons.wait_for_any_long_press().await;
            long_press_fired.set(true);
            if fader_moved_during_press.get() {
                continue;
            }

            if chan == 0 {
                let idx = storage.modify_and_save(|s| {
                    s.scale_idx = cycle_scale_idx(s.scale_idx, shift);
                    s.scale_idx
                });
                leds.set(0, Led::Button, set_color(idx), Brightness::High);
            } else if chan == 1 {
                let mode = storage.modify_and_save(|s| {
                    s.voicing_mode = cycle_voicing(s.voicing_mode, shift);
                    s.voicing_mode
                });
                leds.set(
                    1,
                    Led::Button,
                    voicing_color(mode, led_color),
                    Brightness::High,
                );
            }
        }
    };

    let led_handler = async {
        while true {
            app.delay_millis(8).await;
            let layer = if buttons.is_shift_pressed() && !buttons.is_button_pressed(0) {
                LatchLayer::Alt
            } else if !buttons.is_shift_pressed() && buttons.is_button_pressed(0) {
                LatchLayer::Third
            } else {
                LatchLayer::Main
            };
            glob_latch_layer.set(layer);

            let duck_active = {
                let d = glob_button_duck.get();
                if d > 0 {
                    glob_button_duck.set(d.saturating_sub(8));
                    true
                } else {
                    false
                }
            };

            if glob_muted.get() {
                leds.unset_chan(0);
                leds.unset_chan(1);
                continue;
            }

            let (scale_idx, voicing_mode, f_main, f_alt, f_third) = storage.query(|s| {
                (
                    s.scale_idx,
                    s.voicing_mode,
                    s.fader_main,
                    s.fader_alt,
                    s.fader_third,
                )
            });
            let scale_col = set_color(scale_idx);
            let voice_col = voicing_color(voicing_mode, led_color);
            let gate = glob_gate_on.get();
            let pitch_m = split_unsigned_value(glob_pitch_led.get());
            let burst_m = split_unsigned_value(glob_burst_led.get());

            let layer_val = |chan: usize| match layer {
                LatchLayer::Main => f_main[chan],
                LatchLayer::Alt => f_alt[chan],
                LatchLayer::Third => f_third[chan],
            };

            for chan in 0..2 {
                let val = layer_val(chan);
                let cycle_col = if chan == 0 { scale_col } else { voice_col };
                let (meter_col, btn_col) = match layer {
                    LatchLayer::Alt => (Color::White, Color::White),
                    LatchLayer::Third => (cycle_col, cycle_col),
                    LatchLayer::Main => (led_color, cycle_col),
                };

                match layer {
                    LatchLayer::Alt | LatchLayer::Third => {
                        let m = split_unsigned_value(val);
                        leds.set(chan, Led::Top, meter_col, Brightness::Custom(m[0]));
                        leds.set(chan, Led::Bottom, meter_col, Brightness::Custom(m[1]));
                    }
                    LatchLayer::Main => {
                        if gate {
                            let m = if chan == 0 { burst_m } else { pitch_m };
                            leds.set(chan, Led::Top, meter_col, Brightness::Custom(m[0]));
                            leds.set(chan, Led::Bottom, meter_col, Brightness::Custom(m[1]));
                        } else {
                            leds.unset(chan, Led::Top);
                            leds.unset(chan, Led::Bottom);
                        }
                    }
                }

                let bright = if duck_active {
                    Brightness::Off
                } else {
                    signal_brightness(val, false)
                };
                leds.set(chan, Led::Button, btn_col, bright);
            }
        }
    };

    let cv_handler = async {
        while true {
            app.delay_millis(1).await;
            let in_val = attenuate_bipolar(cv_in.get_value(), cv_att);
            glob_cv_val.set(in_val);
        }
    };

    let scene_handler = async {
        while true {
            match app.wait_for_scene_event().await {
                SceneEvent::LoadScene(scene) => {
                    storage.load_from_scene(scene).await;
                    let muted = storage.query(|s| s.muted);
                    glob_muted.set(muted);
                    if muted {
                        pending_silence.set(true);
                        leds.unset_chan(0);
                        leds.unset_chan(1);
                    } else {
                        let (si, vm) = storage.query(|s| (s.scale_idx, s.voicing_mode));
                        leds.set(0, Led::Button, set_color(si), LED_BRIGHTNESS);
                        leds.set(1, Led::Button, voicing_color(vm, led_color), LED_BRIGHTNESS);
                    }
                }
                SceneEvent::SaveScene(scene) => storage.save_to_scene(scene).await,
            }
        }
    };

    join(
        clock_drain,
        join5(
            join(fut_engine, fut_voice),
            fader_handler,
            join(button_handler, long_press_handler),
            join(led_handler, cv_handler),
            scene_handler,
        ),
    )
    .await;
}
