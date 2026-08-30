use embassy_futures::{
    join::{join, join3, join4},
    select::{select, select3},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use heapless::Vec;
use serde::{Deserialize, Serialize};

use libfp::{
    ext::FromValue,
    latch::LatchLayer,
    utils::{attenuverter, clickless, midi_gate, split_unsigned_value},
    AppIcon, Brightness, ClockDivision, Color, Config, Curve, MidiCc, MidiChannel, MidiMode,
    MidiNote, MidiOut,
    Param, Range, Value, APP_MAX_PARAMS,
};

use crate::{
    app::{
        App, AppParams, AppStorage, ClockEvent, Led, Leds, ManagedStorage, ParamStore, SceneEvent,
    },
    tasks::leds::LedMode,
};

use self::led_fx::{color_hue, hsv_to_rgb};
use self::morph::{morph_color, morph_sample, MorphChaos};

pub const CHANNELS: usize = 2;
pub const PARAMS: usize = 9;

const BUTTON_BRIGHTNESS: Brightness = Brightness::Mid;
const IN_DEADBAND: u16 = 24;
const IN_IDLE_MS: u16 = 1200;
const BUTTON_FLASH_MS: u16 = 850;
/// After mode long-press release in LFO mode, hold mode hue then ease to morph.
const MODE_TO_MORPH_MS: u16 = 350;
const LFO_WARP: u16 = 0;
const LFO_SYMMETRY: u16 = 2048;
const LFO_DIVISIONS: usize = 9;
const LFO_SKEW: u16 = 2048;
const FADER_MOVE_THRESH: u16 = 64;
const FILTER_FS: f32 = 1000.0;
const CUTOFF_MIN_HZ: f32 = 0.1;
const CUTOFF_MAX_HZ: f32 = 200.0;

const CLOCK_DIVISIONS: [u32; 13] = [384, 192, 96, 48, 24, 16, 12, 8, 6, 4, 3, 2, 1];

fn division_at(fader: u16, count: usize) -> u32 {
    let count = count.clamp(1, CLOCK_DIVISIONS.len());
    let idx = (fader.min(4095) as usize * count / 4096).min(count - 1);
    CLOCK_DIVISIONS[idx]
}

fn lfo_step(speed: u16) -> f32 {
    (Curve::Exponential.at(speed) as f32 + 2047.0 - 2047.0) * 0.015 + 0.0682
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

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
enum FilterMode {
    Lp = 0,
    Hp = 1,
    Bp = 2,
    Notch = 3,
}

impl FilterMode {
    fn from_usize(v: usize) -> Self {
        match v {
            1 => FilterMode::Hp,
            2 => FilterMode::Bp,
            3 => FilterMode::Notch,
            _ => FilterMode::Lp,
        }
    }

    fn next(self) -> Self {
        match self {
            FilterMode::Lp => FilterMode::Hp,
            FilterMode::Hp => FilterMode::Bp,
            FilterMode::Bp => FilterMode::Notch,
            FilterMode::Notch => FilterMode::Lp,
        }
    }

    fn color(self, base: u16) -> Color {
        let offset = match self {
            FilterMode::Lp => 0,
            FilterMode::Hp => 60,
            FilterMode::Bp => 120,
            FilterMode::Notch => 180,
        };
        let hue = ((base as i32 + offset) % 360) as u16;
        let (r, g, b) = hsv_to_rgb(hue);
        Color::Custom(r, g, b)
    }
}

struct SvfState {
    low: f32,
    band: f32,
}

impl SvfState {
    fn new() -> Self {
        Self {
            low: 0.0,
            band: 0.0,
        }
    }

    fn step(&mut self, input: f32, fc_hz: f32, damping: f32) -> (f32, f32, f32, f32) {
        // Keep q high enough and f below q so Chamberlin stays non-oscillating.
        let q = damping.clamp(0.55, 2.0);
        let mut f = 2.0 * libm::sinf(core::f32::consts::PI * fc_hz / FILTER_FS);
        f = f.clamp(0.0001, 1.0).min(q * 0.9);

        self.low += f * self.band;
        let high = input - self.low - q * self.band;
        self.band += f * high;

        // Soft state clamp — runaway recovery if a bad coeff pair slips through.
        self.low = self.low.clamp(-4.0, 4.0);
        self.band = self.band.clamp(-4.0, 4.0);

        let lp = self.low;
        let hp = high;
        let bp = self.band;
        let notch = lp + hp;
        (lp, hp, bp, notch)
    }
}

fn fader_to_cutoff(fader: u16) -> f32 {
    let t = fader as f32 / 4095.0;
    CUTOFF_MIN_HZ * libm::powf(CUTOFF_MAX_HZ / CUTOFF_MIN_HZ, t)
}

fn fader_to_damping(fader: u16) -> f32 {
    // Fader up → more resonance (lower damping), but floor at 0.55 (no self-osc).
    let t = fader as f32 / 4095.0;
    2.0 - t * 1.45
}

/// `mix` 0 = fully dry, 4095 = fully wet.
fn dry_wet_mix(dry: u16, wet: u16, mix: u16) -> u16 {
    let w = mix as u32;
    let d = 4095u32 - w;
    ((dry as u32 * d + wet as u32 * w) / 4095) as u16
}

fn center_sample(raw: u16, bipolar: bool) -> f32 {
    if bipolar {
        (raw as f32 - 2047.0) / 2047.0
    } else {
        raw as f32 / 4095.0
    }
}

fn uncenter_sample(val: f32, bipolar: bool) -> u16 {
    if bipolar {
        (val * 2047.0 + 2047.0).clamp(0.0, 4095.0) as u16
    } else {
        (val * 4095.0).clamp(0.0, 4095.0) as u16
    }
}

fn mute_level(bipolar: bool) -> u16 {
    if bipolar {
        2047
    } else {
        0
    }
}

pub static CONFIG: Config<PARAMS> = Config::new(
    "Semmy",
    "CV state-variable filter (LP/HP/BP/Notch)",
    Color::Cyan,
    AppIcon::Attenuate,
)
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
.add_param(Param::Range {
    name: "Range",
    variants: &[Range::_0_10V, Range::_Neg5_5V],
})
.add_param(Param::Enum {
    name: "Mode",
    variants: &["LP", "HP", "BP", "Notch"],
})
.add_param(Param::Enum {
    name: "LFO Speed",
    variants: &["Normal", "Slow", "Slowest"],
})
.add_param(Param::MidiMode)
.add_param(Param::MidiChannel {
    name: "MIDI Channel",
})
.add_param(Param::MidiNote { name: "Base Note" })
.add_param(Param::MidiCc { name: "CC number" })
.add_param(Param::MidiOut);

pub struct Params {
    color: Color,
    range: Range,
    mode: FilterMode,
    lfo_speed_mult: usize,
    midi_mode: MidiMode,
    midi_channel: MidiChannel,
    midi_note: MidiNote,
    midi_cc: MidiCc,
    midi_out: MidiOut,
}

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.len() < PARAMS {
            return None;
        }
        Some(Self {
            color: Color::from_value(values[0]),
            range: Range::from_value(values[1]),
            mode: FilterMode::from_usize(usize::from_value(values[2])),
            lfo_speed_mult: usize::from_value(values[3]),
            midi_mode: MidiMode::from_value(values[4]),
            midi_channel: MidiChannel::from_value(values[5]),
            midi_note: MidiNote::from_value(values[6]),
            midi_cc: MidiCc::from_value(values[7]),
            midi_out: MidiOut::from_value(values[8]),
        })
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        let mut vec = Vec::new();
        vec.push(self.color.into()).unwrap();
        vec.push(self.range.into()).unwrap();
        vec.push(Value::Enum(self.mode as u8 as usize)).unwrap();
        vec.push(Value::Enum(self.lfo_speed_mult)).unwrap();
        vec.push(self.midi_mode.into()).unwrap();
        vec.push(self.midi_channel.into()).unwrap();
        vec.push(self.midi_note.into()).unwrap();
        vec.push(self.midi_cc.into()).unwrap();
        vec.push(self.midi_out.into()).unwrap();
        vec
    }
}

#[derive(Serialize, Deserialize)]
pub struct Storage {
    fader_saved: [u16; 2],
    att_saved: u16,
    offset_saved: u16,
    mode: u8,
    muted: bool,
    lfo_speed: u16,
    morph: u16,
    lfo_clocked: bool,
    /// 0 = dry (pre-filter), 4095 = wet (filtered). Default full wet.
    dry_wet_saved: u16,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            fader_saved: [1500, 2000],
            att_saved: 4095,
            offset_saved: 2047,
            mode: FilterMode::Lp as u8,
            muted: false,
            lfo_speed: 2000,
            morph: 0,
            lfo_clocked: false,
            dry_wet_saved: 4095,
        }
    }
}

impl AppStorage for Storage {}

#[embassy_executor::task(pool_size = 16 / CHANNELS)]
pub async fn wrapper(app: App<CHANNELS>, exit_signal: &'static Signal<NoopRawMutex, bool>) {
    let param_store = ParamStore::<Params>::new(
        app.app_id,
        app.layout_id,
        Params {
            color: Color::Cyan,
            range: Range::_Neg5_5V,
            mode: FilterMode::Lp,
            lfo_speed_mult: 0,
            midi_mode: MidiMode::default(),
            midi_channel: MidiChannel::default(),
            midi_note: MidiNote::from(48),
            midi_cc: MidiCc::from(32u8.saturating_add(app.start_channel as u8)),
            midi_out: MidiOut::default(),
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

fn paint_out_meter(leds: &Leds<CHANNELS>, color: Color, level: u16) {
    let parts = split_unsigned_value(level);
    leds.set(1, Led::Top, color, Brightness::Custom(parts[0]));
    leds.set(1, Led::Bottom, color, Brightness::Custom(parts[1]));
}

pub async fn run(
    app: &App<CHANNELS>,
    params: &ParamStore<Params>,
    storage: &ManagedStorage<Storage>,
) {
    let (led_color, range, midi_out, midi_chan) =
        params.query(|p| (p.color, p.range, p.midi_out, p.midi_channel));

    let buttons = app.use_buttons();
    let faders = app.use_faders();
    let leds = app.use_leds();
    let input = app.make_in_jack(0, range).await;
    let output = app.make_out_jack(1, range).await;
    let midi = app.use_midi_output(midi_out, midi_chan, false);

    // Subscribe only after jack setup: an undrained CLOCK_PUBSUB slot fills
    // while parked on MAX and stalls Start/Reset (`publish().await`).
    let mut clock = app.use_clock();

    let die = app.use_die();
    // u64::MAX = no tick yet (sentinel until first ClockEvent::Tick).
    let glob_ticks = app.make_global(u64::MAX);
    let glob_lfo_active = app.make_global(true);
    let glob_lfo_pos = app.make_global(0.0f32);
    let glob_lfo_step = app.make_global(0.0682f32);
    let glob_div = app.make_global(24u32);
    let glob_chaos = app.make_global(MorphChaos::new());
    let glob_muted = app.make_global(storage.query(|s| s.muted));
    let glob_mode =
        app.make_global(FilterMode::from_usize(storage.query(|s| s.mode as usize)));
    let glob_mode_preview = app.make_global(None::<FilterMode>);
    let glob_mode_to_morph = app.make_global(0u16);
    let glob_base_hue = app.make_global(color_hue(led_color));
    let glob_btn_flash = app.make_global([0u16; 2]);
    let long_press_fired = app.make_global(false);
    let fader_moved_during_hold = app.make_global(false);
    let fader_at_down = app.make_global([0u16; 2]);

    let mut svf = SvfState::new();
    let mut prev_att = storage.query(|s| s.att_saved);
    let mut prev_offset = storage.query(|s| s.offset_saved);

    let time_calc = || {
        let speed = storage.query(|s| s.lfo_speed);
        glob_lfo_step.set(lfo_step(speed));
        glob_div.set(division_at(speed, LFO_DIVISIONS));
    };
    time_calc();

    let initial_mode = glob_mode.get();
    if !glob_muted.get() {
        leds.set(
            0,
            Led::Button,
            initial_mode.color(glob_base_hue.get()),
            BUTTON_BRIGHTNESS,
        );
    }

    let fut1 = async {
        let mut prev_raw_input = input.get_value();
        let mut in_idle_ms = IN_IDLE_MS;
        let mut last_tick = glob_ticks.get();
        let mut ms_since_tick = 0u16;
        let mut tick_period_ms = 21u16;
        let mut last_cc = u16::MAX;
        let mut note_on = false;
        let mut current_note = MidiNote::default();
        let mut prev_midi_mode = params.query(|p| p.midi_mode);

        loop {
            app.delay_millis(1).await;

            glob_base_hue.set(color_hue(params.query(|p| p.color)));
            let base = glob_base_hue.get();
            let bipolar = range.is_bipolar();

            let raw_input = input.get_value();
            if raw_input.abs_diff(prev_raw_input) > IN_DEADBAND {
                in_idle_ms = 0;
            } else {
                in_idle_ms = in_idle_ms.saturating_add(1).min(IN_IDLE_MS);
            }
            prev_raw_input = raw_input;

            let lfo_active = in_idle_ms >= IN_IDLE_MS;
            glob_lfo_active.set(lfo_active);

            let flash = glob_btn_flash.modify(|f| {
                let mut arr = *f;
                for ms in arr.iter_mut() {
                    *ms = ms.saturating_sub(1);
                }
                arr
            });
            let mode_to_morph = glob_mode_to_morph.modify(|ms| ms.saturating_sub(1));

            let (cutoff_fader, q_fader, morph, lfo_clocked) =
                storage.query(|s| (s.fader_saved[0], s.fader_saved[1], s.morph, s.lfo_clocked));

            let tick = glob_ticks.get();
            if tick != last_tick {
                if (1..500).contains(&ms_since_tick) && tick > last_tick {
                    tick_period_ms = ms_since_tick;
                }
                last_tick = tick;
                ms_since_tick = 0;
            } else {
                ms_since_tick = ms_since_tick.saturating_add(1);
            }

            let lfo_speed_mult = 2u32.pow(params.query(|p| p.lfo_speed_mult).min(31) as u32);

            let source_u16 = if lfo_active {
                time_calc();

                let next_pos = if lfo_clocked {
                    if tick == u64::MAX {
                        0.0
                    } else {
                        let ticks_per_cycle = (glob_div.get() as u64)
                            .saturating_mul(lfo_speed_mult as u64)
                            .max(1);
                        let frac = (ms_since_tick as f32 / tick_period_ms.max(1) as f32).min(1.0);
                        let pos_ticks = (tick % ticks_per_cycle) as f32 + frac;
                        (pos_ticks * 4096.0 / ticks_per_cycle as f32) % 4096.0
                    }
                } else {
                    let step = glob_lfo_step.get() / lfo_speed_mult as f32;
                    let pos = glob_lfo_pos.get();
                    (pos + step) % 4096.0
                };

                let mut chaos = glob_chaos.get();
                chaos.tick_walks(&die);
                let sample = morph_sample(
                    next_pos as usize,
                    morph,
                    (LFO_SKEW, LFO_WARP, LFO_SYMMETRY),
                    0,
                    &mut chaos,
                    &die,
                );
                glob_chaos.set(chaos);
                glob_lfo_pos.set(next_pos);
                sample
            } else {
                raw_input
            };

            let centered = center_sample(source_u16, bipolar);
            let fc = fader_to_cutoff(cutoff_fader);
            let damping = fader_to_damping(q_fader);
            let (lp, hp, bp, notch) = svf.step(centered, fc, damping);

            let mode = glob_mode_preview
                .get()
                .unwrap_or_else(|| glob_mode.get());
            let filtered = match mode {
                FilterMode::Lp => lp,
                FilterMode::Hp => hp,
                FilterMode::Bp => bp,
                FilterMode::Notch => notch,
            };

            let filtered_u16 = uncenter_sample(filtered, bipolar);
            let dry_wet = storage.query(|s| s.dry_wet_saved);
            let mixed = dry_wet_mix(source_u16, filtered_u16, dry_wet);

            prev_att = clickless(prev_att, storage.query(|s| s.att_saved));
            prev_offset = clickless(prev_offset, storage.query(|s| s.offset_saved));
            let att = Curve::Deadzone.at(prev_att);
            let offset = Curve::Deadzone.at(prev_offset) as i32 - 2047;

            let outval = if glob_muted.get() {
                mute_level(bipolar)
            } else {
                ((attenuverter(mixed, att) as i32 + offset).clamp(0, 4095)) as u16
            };

            output.set_value(outval);

            if midi_out.is_some() {
                let (midi_mode, midi_cc, base_note) =
                    params.query(|p| (p.midi_mode, p.midi_cc, p.midi_note));
                if midi_mode != prev_midi_mode {
                    if prev_midi_mode == MidiMode::Note && note_on {
                        midi.send_note_off(current_note).await;
                        note_on = false;
                    }
                    if midi_mode == MidiMode::Cc {
                        last_cc = u16::MAX;
                    }
                    prev_midi_mode = midi_mode;
                }
                match midi_mode {
                    MidiMode::Cc => {
                        let gate_val = midi_gate(outval, false);
                        if gate_val != last_cc {
                            midi.send_cc(midi_cc, outval).await;
                            last_cc = gate_val;
                        }
                    }
                    MidiMode::Note => {
                        let muted = glob_muted.get();
                        let note = {
                            let semitones = (outval as i32 * 24 / 4095) as i8;
                            let mut n = base_note;
                            n.transpose(semitones)
                        };
                        if muted {
                            if note_on {
                                midi.send_note_off(current_note).await;
                                note_on = false;
                            }
                        } else if note != current_note {
                            if note_on {
                                midi.send_note_off(current_note).await;
                            }
                            midi.send_note_on(note, 4095).await;
                            current_note = note;
                            note_on = true;
                        }
                    }
                }
            }

            let shift = buttons.is_shift_pressed();
            let btn0 = buttons.is_button_pressed(0);
            let btn1 = buttons.is_button_pressed(1);
            let latch = if shift && !btn0 && !btn1 {
                LatchLayer::Alt
            } else if !shift && (btn0 || btn1) {
                LatchLayer::Third
            } else {
                LatchLayer::Main
            };

            let wave_bright = signal_brightness(outval, bipolar);
            let muted = glob_muted.get();

            match latch {
                LatchLayer::Main => {
                    paint_out_meter(&leds, led_color, outval);

                    // Ch1 button always breathes with the filtered output.
                    if muted {
                        leds.unset(1, Led::Button);
                    } else {
                        leds.set(1, Led::Button, led_color, wave_bright);
                    }

                    if flash[0] == 0 {
                        if muted {
                            leds.unset(0, Led::Button);
                            leds.unset(0, Led::Top);
                            leds.unset(0, Led::Bottom);
                        } else if let Some(preview) = glob_mode_preview.get() {
                            // Long-press accepted: solid mode hue until release.
                            let mode_c = preview.color(base);
                            leds.set(0, Led::Button, mode_c, BUTTON_BRIGHTNESS);
                        } else if lfo_active {
                            let morph_c = morph_color(morph);
                            let morph_bright = ((morph / 16) as u8).max(32);
                            if mode_to_morph > 0 {
                                // Ease from mode hue toward morph after release.
                                let mode_c = mode.color(base);
                                let t = mode_to_morph;
                                let half = MODE_TO_MORPH_MS / 2;
                                if t > half {
                                    leds.set(0, Led::Button, mode_c, BUTTON_BRIGHTNESS);
                                } else {
                                    let fade =
                                        ((t as u32 * 255) / half.max(1) as u32) as u8;
                                    leds.set(
                                        0,
                                        Led::Button,
                                        morph_c,
                                        Brightness::Custom(fade.max(32)),
                                    );
                                }
                            } else {
                                leds.set(0, Led::Button, morph_c, wave_bright);
                            }
                            leds.set(0, Led::Top, morph_c, Brightness::Custom(morph_bright));
                            let in_led = split_unsigned_value(source_u16);
                            leds.set(0, Led::Bottom, led_color, Brightness::Custom(in_led[1]));
                        } else {
                            let mode_c = mode.color(base);
                            leds.set(0, Led::Button, mode_c, wave_bright);
                            let in_led = split_unsigned_value(source_u16);
                            leds.set(0, Led::Top, led_color, Brightness::Custom(in_led[0]));
                            leds.set(0, Led::Bottom, led_color, Brightness::Custom(in_led[1]));
                        }
                    }
                }
                LatchLayer::Alt => {
                    // Shift: Speed (LFO) / Offset (CV) on ch0, Att on ch1 — red dim gradients.
                    if lfo_active {
                        let speed = storage.query(|s| s.lfo_speed);
                        let speed_bright = (speed / 16) as u8;
                        leds.set(0, Led::Top, Color::Red, Brightness::Custom(speed_bright));
                        leds.unset(0, Led::Bottom);
                        if flash[0] == 0 {
                            leds.set(
                                0,
                                Led::Button,
                                Color::Red,
                                Brightness::Custom(speed_bright.max(32)),
                            );
                        }
                    } else {
                        let off = storage.query(|s| s.offset_saved);
                        let off_led = split_unsigned_value(Curve::Deadzone.at(off));
                        leds.set(0, Led::Top, Color::Red, Brightness::Custom(off_led[0]));
                        leds.set(0, Led::Bottom, Color::Red, Brightness::Custom(off_led[1]));
                        if flash[0] == 0 {
                            let off_b = ((off / 16) as u8).max(32);
                            leds.set(0, Led::Button, Color::Red, Brightness::Custom(off_b));
                        }
                    }
                    let att_led = split_unsigned_value(Curve::Deadzone.at(prev_att));
                    leds.set(1, Led::Top, Color::Red, Brightness::Custom(att_led[0]));
                    leds.set(1, Led::Bottom, Color::Red, Brightness::Custom(att_led[1]));
                    let att_b = ((prev_att / 16) as u8).max(32);
                    leds.set(1, Led::Button, Color::Red, Brightness::Custom(att_b));
                }
                LatchLayer::Third => {
                    // Btn0 hold: freeze ch0 LEDs until long→mode hue or fader scrub (Third edit).
                    // Short release (no long) → mute elsewhere; don’t blank the button on down.
                    if btn0 {
                        if let Some(preview) = glob_mode_preview.get() {
                            if flash[0] == 0 {
                                leds.set(
                                    0,
                                    Led::Button,
                                    preview.color(base),
                                    BUTTON_BRIGHTNESS,
                                );
                            }
                        } else if fader_moved_during_hold.get() {
                            if lfo_active {
                                let morph_c = morph_color(morph);
                                let morph_bright = ((morph / 16) as u8).max(32);
                                leds.set(0, Led::Top, morph_c, Brightness::Custom(morph_bright));
                                leds.unset(0, Led::Bottom);
                                if flash[0] == 0 {
                                    leds.set(
                                        0,
                                        Led::Button,
                                        morph_c,
                                        Brightness::Custom(morph_bright),
                                    );
                                }
                            } else {
                                let cut_b = ((cutoff_fader / 16) as u8).max(32);
                                leds.set(0, Led::Top, led_color, Brightness::Custom(cut_b));
                                leds.unset(0, Led::Bottom);
                                if flash[0] == 0 {
                                    leds.set(
                                        0,
                                        Led::Button,
                                        mode.color(base),
                                        Brightness::Custom(cut_b),
                                    );
                                }
                            }
                        }
                        // else: freeze — leave ch0 Button/Top/Bottom as they were at press.
                    }
                    if btn1 || !btn0 {
                        let mix = storage.query(|s| s.dry_wet_saved);
                        let mix_b = ((mix / 16) as u8).max(32);
                        // Cyan = wetter; dimmer toward dry.
                        leds.set(1, Led::Top, Color::Cyan, Brightness::Custom(mix_b));
                        leds.unset(1, Led::Bottom);
                        leds.set(1, Led::Button, Color::Cyan, Brightness::Custom(mix_b));
                        if !btn0 {
                            paint_out_meter(&leds, led_color, outval);
                        }
                    } else {
                        paint_out_meter(&leds, led_color, outval);
                        if muted {
                            leds.unset(1, Led::Button);
                        } else {
                            leds.set(1, Led::Button, led_color, wave_bright);
                        }
                    }
                }
            }
        }
    };

    let fut2 = async {
        let mut latch = [
            app.make_latch(faders.get_value_at(0)),
            app.make_latch(faders.get_value_at(1)),
        ];

        loop {
            let chan = faders.wait_for_any_change().await;
            let fader_val = faders.get_value_at(chan);

            if buttons.is_button_pressed(chan) && !buttons.is_shift_pressed() {
                let at_down = fader_at_down.get()[chan];
                if fader_val.abs_diff(at_down) > FADER_MOVE_THRESH {
                    fader_moved_during_hold.set(true);
                }
            }

            let latch_layer = if buttons.is_shift_pressed() && !buttons.is_button_pressed(chan) {
                LatchLayer::Alt
            } else if !buttons.is_shift_pressed() && buttons.is_button_pressed(chan) {
                LatchLayer::Third
            } else {
                LatchLayer::Main
            };

            let lfo_active = glob_lfo_active.get();

            let target_value = if chan == 0 {
                match (lfo_active, latch_layer) {
                    (true, LatchLayer::Main) => storage.query(|s| s.fader_saved[0]),
                    (true, LatchLayer::Alt) => storage.query(|s| s.lfo_speed),
                    (true, LatchLayer::Third) => storage.query(|s| s.morph),
                    (false, LatchLayer::Main) => storage.query(|s| s.fader_saved[0]),
                    (false, LatchLayer::Alt) => storage.query(|s| s.offset_saved),
                    (false, LatchLayer::Third) => 0,
                }
            } else {
                match (lfo_active, latch_layer) {
                    (_, LatchLayer::Main) => storage.query(|s| s.fader_saved[1]),
                    (_, LatchLayer::Alt) => storage.query(|s| s.att_saved),
                    (_, LatchLayer::Third) => storage.query(|s| s.dry_wet_saved),
                }
            };

            if let Some(new_value) = latch[chan].update(fader_val, latch_layer, target_value) {
                match chan {
                    0 => match (lfo_active, latch_layer) {
                        (true, LatchLayer::Main) => {
                            storage.modify_and_save(|s| s.fader_saved[0] = new_value);
                        }
                        (true, LatchLayer::Alt) => {
                            storage.modify_and_save(|s| s.lfo_speed = new_value);
                            time_calc();
                        }
                        (true, LatchLayer::Third) => {
                            storage.modify_and_save(|s| s.morph = new_value);
                        }
                        (false, LatchLayer::Main) => {
                            storage.modify_and_save(|s| s.fader_saved[0] = new_value);
                        }
                        (false, LatchLayer::Alt) => {
                            storage.modify_and_save(|s| s.offset_saved = new_value);
                        }
                        (false, LatchLayer::Third) => {}
                    },
                    1 => match latch_layer {
                        LatchLayer::Main => {
                            storage.modify_and_save(|s| s.fader_saved[1] = new_value);
                        }
                        LatchLayer::Alt => {
                            storage.modify_and_save(|s| s.att_saved = new_value);
                        }
                        LatchLayer::Third => {
                            storage.modify_and_save(|s| s.dry_wet_saved = new_value);
                        }
                    },
                    _ => {}
                }
            }
        }
    };

    let button_down = async {
        loop {
            let (chan, _) = buttons.wait_for_any_down().await;
            fader_at_down.modify(|a| {
                let mut arr = *a;
                arr[chan] = faders.get_value_at(chan);
                arr
            });
            fader_moved_during_hold.set(false);
            long_press_fired.set(false);
            glob_mode_preview.set(None);
        }
    };

    let button_long = async {
        loop {
            let (chan, shift) = buttons.wait_for_any_long_press().await;
            if chan != 0 {
                continue;
            }
            long_press_fired.set(true);
            if shift {
                continue;
            }
            // Fader scrub while held → treat as Third-layer edit, not mode cycle.
            if fader_moved_during_hold.get() {
                continue;
            }
            // Commit on long register; solid mode color until release (no blink).
            let next = glob_mode.get().next();
            storage.modify_and_save(|s| s.mode = next as u8);
            glob_mode.set(next);
            glob_mode_to_morph.set(0);
            glob_mode_preview.set(Some(next));
            leds.set(
                0,
                Led::Button,
                next.color(glob_base_hue.get()),
                BUTTON_BRIGHTNESS,
            );
        }
    };

    let button_up = async {
        loop {
            let (chan, shift) = buttons.wait_for_any_up().await;
            if chan != 0 {
                continue;
            }

            let moved = fader_moved_during_hold.get();
            let long = long_press_fired.get();

            if moved {
                glob_mode_preview.set(None);
                continue;
            }

            if shift && glob_lfo_active.get() {
                let clocked = storage.modify_and_save(|s| {
                    s.lfo_clocked = !s.lfo_clocked;
                    s.lfo_clocked
                });
                if clocked {
                    leds.set_mode(0, Led::Button, LedMode::Flash(led_color, Some(4)));
                    glob_btn_flash.modify(|f| {
                        let mut arr = *f;
                        arr[0] = BUTTON_FLASH_MS;
                        arr
                    });
                }
            } else if long && !shift {
                glob_mode_preview.set(None);
                // CV: mode color sticks via normal paint. LFO: ease to morph.
                if glob_lfo_active.get() {
                    glob_mode_to_morph.set(MODE_TO_MORPH_MS);
                }
            } else if !shift {
                let muted = glob_muted.toggle();
                storage.modify_and_save(|s| s.muted = muted);
                if muted {
                    leds.unset(0, Led::Button);
                } else if glob_lfo_active.get() {
                    let morph_c = morph_color(storage.query(|s| s.morph));
                    leds.set(0, Led::Button, morph_c, BUTTON_BRIGHTNESS);
                } else {
                    leds.set(
                        0,
                        Led::Button,
                        glob_mode.get().color(glob_base_hue.get()),
                        BUTTON_BRIGHTNESS,
                    );
                }
            }
        }
    };

    let fut3 = join3(button_down, button_up, button_long);

    let scene_handler = async {
        loop {
            match app.wait_for_scene_event().await {
                SceneEvent::LoadScene(scene) => {
                    storage.load_from_scene(scene).await;
                    let (mode, muted) =
                        storage.query(|s| (FilterMode::from_usize(s.mode as usize), s.muted));
                    glob_mode.set(mode);
                    glob_muted.set(muted);
                    time_calc();
                }
                SceneEvent::SaveScene(scene) => storage.save_to_scene(scene).await,
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

    join4(fut1, fut2, fut3, join(clock_drain, scene_handler)).await;
}

mod led_fx {
    #![allow(dead_code)]

    use libfp::Color;
    use smart_leds::RGB8;

pub fn color_hue(c: Color) -> u16 {
    let RGB8 { r, g, b } = RGB8::from(c);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max == 0 || max - min < 8 {
        return 0;
    }
    let d = (max - min) as i32;
    let (r, g, b, max) = (r as i32, g as i32, b as i32, max as i32);
    let h = if max == r {
        ((g - b) * 60) / d
    } else if max == g {
        120 + ((b - r) * 60) / d
    } else {
        240 + ((r - g) * 60) / d
    };
    ((h % 360) + 360) as u16 % 360
}

/// Integer HSV→RGB with S=V=max. Hue in degrees (0..360).
pub fn hsv_to_rgb(hue: u16) -> (u8, u8, u8) {
    let hue = hue % 360;
    let sector = hue / 60; // 0..=5
    let ramp = ((hue % 60) as u32 * 255 / 59) as u8;
    match sector {
        0 => (255, ramp, 0),
        1 => (255 - ramp, 255, 0),
        2 => (0, 255, ramp),
        3 => (0, 255 - ramp, 255),
        4 => (ramp, 0, 255),
        _ => (255, 0, 255 - ramp),
    }
}
}

mod morph {
    #![allow(dead_code)]

    use libfp::{Color, Waveform};

    use crate::app::Die;

/// Morph continuum: soft waves → stepped/chaos.
/// Indices: 0 Sine, 1 Tri, 2 Saw, 3 Square, 4 Walk, 5 S&H, 6 Noise
pub const MORPH_NODES: usize = 7;
/// Hue (degrees 0–359) for each morph node — full saturation at the node,
/// desaturates toward the next node so the waveform type is always readable.
const NODE_HUES: [u16; MORPH_NODES] = [0, 45, 90, 135, 180, 225, 270];
/// How hard Symmetry leans away from 50% for a given fader offset (< 1 = more extreme).
const SYMMETRY_LEAN_CURVE: f32 = 0.45;

pub fn symmetry_phase(phase: usize, symmetry: u16) -> usize {
    // Piecewise phase remap: center (2048) = balanced halves.
    // Lean curve pulls toward extremes faster so shape changes read clearly live.
    let t = (phase % 4096) as f32 / 4096.0;
    let centered = (symmetry as f32 / 4095.0 - 0.5) * 2.0; // -1..1
    let lean = libm::copysignf(libm::powf(centered.abs(), SYMMETRY_LEAN_CURVE), centered);
    let pw = (0.5 + lean * 0.49).clamp(0.01, 0.99);
    let out = if t < pw {
        t / pw * 0.5
    } else {
        0.5 + (t - pw) / (1.0 - pw) * 0.5
    };
    (out.clamp(0.0, 1.0) * 4095.0) as usize
}

pub fn warp_phase(phase: usize, warp: u16) -> usize {
    if warp == 0 {
        return phase % 4096;
    }
    let t = (phase % 4096) as f32 / 4096.0;
    let amount = warp as f32 / 4095.0;
    // Smoothstep blend toward ease-in/out time feel
    let eased = t * t * (3.0 - 2.0 * t);
    let out = t * (1.0 - amount) + eased * amount;
    (out * 4095.0) as usize
}

pub fn skew_phase(phase: usize, skew: u16) -> usize {
    // Center (2048) = linear; low/high lean soft asymmetry (pow curve)
    let t = (phase % 4096) as f32 / 4096.0;
    let s = (skew as f32 / 4095.0 - 0.5) * 2.0; // -1..1
    let warped = if s >= 0.0 {
        libm::powf(t, 1.0 + s)
    } else {
        1.0 - libm::powf(1.0 - t, 1.0 - s)
    };
    (warped.clamp(0.0, 1.0) * 4095.0) as usize
}

#[derive(Clone, Copy)]
pub struct MorphChaos {
    walk_a: i32,
    walk_b: i32,
    sh_a: u16,
    sh_b: u16,
    sh_bucket_a: u16,
    sh_bucket_b: u16,
}

impl MorphChaos {
    pub fn new() -> Self {
        Self {
            walk_a: 2048,
            walk_b: 2048,
            sh_a: 2048,
            sh_b: 2048,
            sh_bucket_a: 0xffff,
            sh_bucket_b: 0xffff,
        }
    }

    pub fn tick_walks(&mut self, die: &Die) {
        // Gentle drift (~±3 at 1 kHz audio tick).
        let step_a = (die.roll() as i32 % 7) - 3;
        let step_b = (die.roll() as i32 % 7) - 3;
        self.walk_a = (self.walk_a + step_a).clamp(0, 4095);
        self.walk_b = (self.walk_b + step_b).clamp(0, 4095);
    }
}

fn classic_wave(node: usize, phase: usize) -> Option<u16> {
    let w = match node {
        0 => Waveform::Sine,
        1 => Waveform::Triangle,
        2 => Waveform::Saw,
        3 => Waveform::Square,
        _ => return None,
    };
    Some(w.at(phase))
}

fn chaos_sample(node: usize, phase: usize, osc: usize, chaos: &mut MorphChaos, die: &Die) -> u16 {
    match node {
        4 => {
            if osc == 0 {
                chaos.walk_a as u16
            } else {
                chaos.walk_b as u16
            }
        }
        5 => {
            // S&H — new level every 1/16 of the cycle (phase bucket).
            let bucket = (phase / 256) as u16;
            let (sh, last) = if osc == 0 {
                (&mut chaos.sh_a, &mut chaos.sh_bucket_a)
            } else {
                (&mut chaos.sh_b, &mut chaos.sh_bucket_b)
            };
            if bucket != *last {
                *last = bucket;
                *sh = die.roll();
            }
            *sh
        }
        _ => die.roll(),
    }
}

fn node_sample(node: usize, phase: usize, osc: usize, chaos: &mut MorphChaos, die: &Die) -> u16 {
    classic_wave(node, phase).unwrap_or_else(|| chaos_sample(node, phase, osc, chaos, die))
}

/// `form` = (skew, warp, symmetry). `osc` selects which chaos state a stepped
/// node draws from, so two oscillators stay decorrelated.
pub fn morph_sample(
    phase: usize,
    morph: u16,
    form: (u16, u16, u16),
    osc: usize,
    chaos: &mut MorphChaos,
    die: &Die,
) -> u16 {
    let (skew, warp, symmetry) = form;
    let p = skew_phase(symmetry_phase(warp_phase(phase, warp), symmetry), skew);
    let segments = MORPH_NODES - 1;
    let seg_size = 4096 / segments;
    let raw_seg = (morph as usize) / seg_size;
    // Past the last node (morph 4092–4095 due to 4096/6 remainder): pure Noise.
    if raw_seg >= segments {
        return node_sample(MORPH_NODES - 1, p, osc, chaos, die);
    }
    let frac = (morph as usize) % seg_size;
    let a = node_sample(raw_seg, p, osc, chaos, die) as i32;
    let b = node_sample(raw_seg + 1, p, osc, chaos, die) as i32;
    (a + (b - a) * frac as i32 / seg_size as i32).clamp(0, 4095) as u16
}

/// Node-snap morph color: full saturation at each waveform anchor, linearly
/// desaturates toward the next node. The new hue snaps in at full saturation
/// the moment the fader crosses a node boundary. The last node (Noise) has no
/// successor, so its segment stays at full blue saturation throughout.
pub fn morph_color(morph: u16) -> Color {
    let segments = MORPH_NODES - 1; // 6
    let seg_size = 4096 / segments; // 682
    let m = morph.min(4095) as usize;
    let seg = (m / seg_size).min(segments - 1);
    let frac = m % seg_size;
    // Last segment (S&H → Noise): no next node, so Noise blue throughout.
    if seg == segments - 1 {
        let (r, g, b) = hsv_to_rgb(NODE_HUES[MORPH_NODES - 1]);
        return Color::Custom(r, g, b);
    }
    let sat = 255u8.saturating_sub((frac * 255 / seg_size) as u8);
    let (r, g, b) = hsv_to_rgb_sat(NODE_HUES[seg], sat);
    Color::Custom(r, g, b)
}

/// Integer HSV→RGB with V=max and variable saturation (0 = white, 255 = full hue).
fn hsv_to_rgb_sat(hue: u16, sat: u8) -> (u8, u8, u8) {
    let (fr, fg, fb) = hsv_to_rgb(hue);
    let s = sat as u32;
    let w = 255 - s;
    (
        ((fr as u32 * s + 255 * w) / 255) as u8,
        ((fg as u32 * s + 255 * w) / 255) as u8,
        ((fb as u32 * s + 255 * w) / 255) as u8,
    )
}

/// Integer HSV→RGB with S=V=max. Hue in degrees (0..360).
fn hsv_to_rgb(hue: u16) -> (u8, u8, u8) {
    let sector = hue / 60; // 0..=5
                           // Rising/falling ramp within the sector, scaled to 0..=255.
    let ramp = ((hue % 60) as u32 * 255 / 59) as u8;
    match sector {
        0 => (255, ramp, 0),
        1 => (255 - ramp, 255, 0),
        2 => (0, 255, ramp),
        3 => (0, 255 - ramp, 255),
        4 => (ramp, 0, 255),
        _ => (255, 0, 255 - ramp),
    }
}
}

