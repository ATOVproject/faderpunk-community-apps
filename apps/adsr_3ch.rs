use embassy_futures::{
    join::{join, join5},
    select::{select, select3},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use heapless::Vec;
use libfp::{
    ext::FromValue,
    latch::LatchLayer,
    utils::{attenuate, midi_gate},
    AppIcon, Brightness, Color, Config, Curve, MidiCc, MidiChannel, MidiIn, MidiOut, Param, Range,
    Value, APP_MAX_PARAMS,
};
use midly::MidiMessage;
use serde::{Deserialize, Serialize};

use crate::app::{App, AppParams, AppStorage, Led, ManagedStorage, ParamStore, SceneEvent};

pub const CHANNELS: usize = 3;
pub const PARAMS: usize = 5;

/// Channel carrying the gate input jack and the manual gate button.
const GATE_CHAN: usize = 0;
/// Channel carrying the ADSR output jack. Its button also mutes.
const OUT_CHAN: usize = CHANNELS - 2;
/// Channel carrying the ASR output jack.
const ASR_CHAN: usize = CHANNELS - 1;
/// Fader indices.
const ATTACK: usize = 0;
/// Shared by the decay and release stages.
const DECAY_RELEASE: usize = 1;
const SUSTAIN: usize = 2;

/// Gate input thresholds on the 0-10V range (about 1V rising, 0.7V falling).
const GATE_HIGH: u16 = 406;
const GATE_LOW: u16 = 300;

pub static CONFIG: Config<PARAMS> = Config::new(
    "ADSR 3ch",
    "ADSR envelope with decay and release on one fader, plus an ASR output",
    Color::Yellow,
    AppIcon::AdEnv,
)
.add_param(Param::MidiIn)
.add_param(Param::MidiChannel {
    name: "MIDI Channel",
})
.add_param(Param::MidiOut)
.add_param(Param::MidiCc { name: "MIDI CC" })
.add_param(Param::MidiNrpn);

pub struct Params {
    midi_in: MidiIn,
    midi_channel: MidiChannel,
    midi_out: MidiOut,
    midi_cc: MidiCc,
    nrpn: bool,
}

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.len() < PARAMS {
            return None;
        }
        Some(Self {
            midi_in: MidiIn::from_value(values[0]),
            midi_channel: MidiChannel::from_value(values[1]),
            midi_out: MidiOut::from_value(values[2]),
            midi_cc: MidiCc::from_value(values[3]),
            nrpn: bool::from_value(values[4]),
        })
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        let mut vec = Vec::new();
        // PARAMS <= APP_MAX_PARAMS is asserted by Config, so the pushes cannot fail.
        vec.push(self.midi_in.into()).unwrap();
        vec.push(self.midi_channel.into()).unwrap();
        vec.push(self.midi_out.into()).unwrap();
        vec.push(self.midi_cc.into()).unwrap();
        vec.push(Value::MidiNrpn(self.nrpn)).unwrap();
        vec
    }
}

#[derive(Serialize, Deserialize)]
pub struct Storage {
    /// Attack, decay and release, and sustain fader positions, 0-4095.
    fader_saved: [u16; CHANNELS],
    /// Gate stretch for short triggers (AD's minimum gate length), 0-4095.
    min_gate_saved: u16,
    /// Output attenuators (ADSR, then ASR), 0-4095.
    att_saved: [u16; 2],
    /// One curve per channel's button: the ADSR attack, the ADSR decay and
    /// release curve, then the ASR curve (used for both its attack and release).
    curve_saved: [Curve; CHANNELS],
    /// Mute state of the ADSR and ASR outputs.
    muted: [bool; 2],
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            fader_saved: [300, 1500, 2800],
            curve_saved: [Curve::Linear; CHANNELS],
            min_gate_saved: 1,
            att_saved: [4095; 2],
            muted: [false; 2],
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
            midi_in: MidiIn([false, false]),
            midi_channel: MidiChannel::default(),
            midi_out: MidiOut([false, false, false]),
            midi_cc: MidiCc::from(32u8.saturating_add((app.start_channel + OUT_CHAN) as u8)),
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

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// ADSR state machine, advanced once per millisecond. Every ramp runs from
/// where the level currently is, so a retrigger or an early gate-off never
/// jumps, and a ramp covering part of the range takes proportionally less
/// time than the full-scale time the fader sets.
struct Envelope {
    stage: Stage,
    gate: bool,
    level: u16,
    from: u16,
    target: u16,
    elapsed: u32,
    duration: u32,
}

impl Envelope {
    const fn new() -> Self {
        Self {
            stage: Stage::Idle,
            gate: false,
            level: 0,
            from: 0,
            target: 0,
            elapsed: 0,
            duration: 1,
        }
    }

    fn begin(&mut self, stage: Stage, target: u16, full_scale_ms: u32) {
        let distance = (target as i32 - self.level as i32).unsigned_abs();
        self.stage = stage;
        self.from = self.level;
        self.target = target;
        self.elapsed = 0;
        // At least 1 so progress never divides by zero.
        self.duration = (full_scale_ms.saturating_mul(distance) / 4095).max(1);
    }

    /// Move one millisecond along the current ramp. Returns true once it has
    /// arrived at its target.
    fn advance(&mut self, curve: Curve) -> bool {
        self.elapsed = self.elapsed.saturating_add(1).min(self.duration);
        let progress = (self.elapsed * 4095 / self.duration) as u16;
        let shaped = curve.at(progress) as i32;
        let span = self.target as i32 - self.from as i32;
        self.level = (self.from as i32 + span * shaped / 4095).clamp(0, 4095) as u16;
        self.elapsed >= self.duration
    }

    /// `ms` and `curves` hold the attack, decay and release times (full scale)
    /// and curves.
    fn tick(&mut self, gate: bool, ms: [u32; 3], sustain: u16, curves: [Curve; 3]) -> u16 {
        if gate && !self.gate {
            self.begin(Stage::Attack, 4095, ms[0]);
        } else if !gate && self.gate && self.stage != Stage::Idle {
            self.begin(Stage::Release, 0, ms[2]);
        }
        self.gate = gate;

        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                if self.advance(curves[0]) {
                    self.begin(Stage::Decay, sustain, ms[1]);
                }
            }
            Stage::Decay => {
                // Follow the sustain fader while decaying toward it.
                self.target = sustain;
                if self.advance(curves[1]) {
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = sustain,
            Stage::Release => {
                if self.advance(curves[2]) {
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }
}

/// The ADSR's attack, decay and release curves from the per-button curves;
/// decay and release share one.
fn adsr_curves(saved: [Curve; CHANNELS]) -> [Curve; 3] {
    [saved[0], saved[1], saved[1]]
}

/// Full-scale stage time for a fader position, 2 ms to about 8.2 s.
fn stage_ms(fader: u16) -> u32 {
    Curve::Exponential.at(fader) as u32 * 2 + 2
}

fn curve_color(curve: Curve) -> Color {
    match curve {
        Curve::Linear | Curve::Deadzone => Color::Yellow,
        Curve::Logarithmic => Color::Cyan,
        Curve::Exponential => Color::Pink,
    }
}

pub async fn run(
    app: &App<CHANNELS>,
    params: &ParamStore<Params>,
    storage: &ManagedStorage<Storage>,
) {
    let (midi_in, midi_chan, midi_out, midi_cc, nrpn) =
        params.query(|p| (p.midi_in, p.midi_channel, p.midi_out, p.midi_cc, p.nrpn));
    let midi_out_handle = app.use_midi_output(midi_out, midi_chan, nrpn);
    let buttons = app.use_buttons();
    let faders = app.use_faders();
    let leds = app.use_leds();

    let input = app.make_in_jack(GATE_CHAN, Range::_0_10V).await;
    let output = app.make_out_jack(OUT_CHAN, Range::_0_10V).await;
    let asr_output = app.make_out_jack(ASR_CHAN, Range::_0_10V).await;

    // Number of MIDI notes currently held.
    let notes_held = app.make_global(0i32);
    let long_press_fired = app.make_global(false);

    let main_loop = async {
        let mut env = Envelope::new();
        let mut asr = Envelope::new();
        let mut jack_gate = false;
        let mut manual_gate = false;
        let mut raw_gate_was_high = false;
        let mut stretch_left: u32 = 0;
        let mut last_midi_val = u16::MAX;

        loop {
            app.delay_millis(1).await;

            let (saved, curve_saved, min_gate, att, muted) = storage.query(|s| {
                (
                    s.fader_saved,
                    s.curve_saved,
                    s.min_gate_saved,
                    s.att_saved,
                    s.muted,
                )
            });

            let jack = input.get_value();
            if jack >= GATE_HIGH {
                jack_gate = true;
            } else if jack < GATE_LOW {
                jack_gate = false;
            }
            // The manual gate starts with Shift + button 1 and lasts while the
            // button is held, so Shift can be let go to use the main fader layer
            // (e.g. moving Sustain) while it is held.
            if !buttons.is_button_pressed(GATE_CHAN) {
                manual_gate = false;
            } else if buttons.is_shift_pressed() {
                manual_gate = true;
            }
            let raw_gate = jack_gate || manual_gate || notes_held.get() > 0;
            // As in AD, a short trigger is stretched to the minimum gate length.
            if raw_gate && !raw_gate_was_high {
                stretch_left = min_gate as u32 + 10;
            }
            raw_gate_was_high = raw_gate;
            stretch_left = stretch_left.saturating_sub(1);
            let gate = raw_gate || stretch_left > 0;

            let ms = [
                stage_ms(saved[ATTACK]),
                stage_ms(saved[DECAY_RELEASE]),
                stage_ms(saved[DECAY_RELEASE]),
            ];
            let level = env.tick(gate, ms, saved[SUSTAIN], adsr_curves(curve_saved));
            // ASR: the same attack and release times, with its own curve for both,
            // held at full scale while the gate is high.
            let asr_level = asr.tick(gate, ms, 4095, [curve_saved[ASR_CHAN]; 3]);
            let out = if muted[0] {
                0
            } else {
                attenuate(level, att[0])
            };
            let asr_out = if muted[1] {
                0
            } else {
                attenuate(asr_level, att[1])
            };
            output.set_value(out);
            asr_output.set_value(asr_out);

            if midi_out.is_some() {
                let midi_val = midi_gate(out, nrpn);
                if midi_val != last_midi_val {
                    midi_out_handle.send_cc(midi_cc, out).await;
                    last_midi_val = midi_val;
                }
            }

            // Fader positions as dim top LEDs, the active stage in white.
            let active = match env.stage {
                Stage::Attack => Some(ATTACK),
                Stage::Decay | Stage::Release => Some(DECAY_RELEASE),
                Stage::Sustain => Some(SUSTAIN),
                Stage::Idle => None,
            };
            for chan in 0..CHANNELS {
                let color = curve_saved
                    .get(chan)
                    .map_or(Color::Blue, |c| curve_color(*c));
                if active == Some(chan) {
                    leds.set(chan, Led::Top, Color::White, Brightness::High);
                } else {
                    leds.set(
                        chan,
                        Led::Top,
                        color,
                        Brightness::Custom((saved[chan] / 32) as u8),
                    );
                }
                // Button colour shows the channel's curve, as in AD. A muted
                // output's button goes dark.
                let chan_muted = (chan == OUT_CHAN && muted[0]) || (chan == ASR_CHAN && muted[1]);
                match curve_saved.get(chan) {
                    Some(c) if !chan_muted => {
                        leds.set(chan, Led::Button, curve_color(*c), Brightness::Mid)
                    }
                    _ => leds.unset(chan, Led::Button),
                }
            }
            leds.set(
                OUT_CHAN,
                Led::Bottom,
                Color::White,
                Brightness::Custom((out / 16) as u8),
            );
            leds.set(
                ASR_CHAN,
                Led::Bottom,
                Color::White,
                Brightness::Custom((asr_out / 16) as u8),
            );
            if buttons.is_shift_pressed() {
                // Alt layer: minimum gate length and the two attenuators.
                for (chan, value) in [
                    (GATE_CHAN, min_gate),
                    (OUT_CHAN, att[0]),
                    (ASR_CHAN, att[1]),
                ] {
                    leds.set(
                        chan,
                        Led::Top,
                        Color::Red,
                        Brightness::Custom((value / 16) as u8),
                    );
                }
            }
            leds.set(
                GATE_CHAN,
                Led::Bottom,
                Color::Red,
                if gate {
                    Brightness::High
                } else {
                    Brightness::Low
                },
            );
        }
    };

    let fader_handler = async {
        let mut latch =
            core::array::from_fn::<_, CHANNELS, _>(|n| app.make_latch(faders.get_value_at(n)));

        loop {
            let chan = faders.wait_for_any_change().await;
            if chan >= CHANNELS {
                continue;
            }
            // Shift + fader is the alt layer, as in AD: the gate channel sets the
            // minimum gate length, each output's channel attenuates that output.
            if buttons.is_shift_pressed() {
                let target = match chan {
                    GATE_CHAN => storage.query(|s| s.min_gate_saved),
                    OUT_CHAN => storage.query(|s| s.att_saved[0]),
                    ASR_CHAN => storage.query(|s| s.att_saved[1]),
                    _ => continue,
                };
                if let Some(new_value) =
                    latch[chan].update(faders.get_value_at(chan), LatchLayer::Alt, target)
                {
                    storage.modify_and_save(|s| match chan {
                        GATE_CHAN => s.min_gate_saved = new_value,
                        OUT_CHAN => s.att_saved[0] = new_value,
                        _ => s.att_saved[1] = new_value,
                    });
                }
                continue;
            }
            let target = storage.query(|s| s.fader_saved[chan]);
            if let Some(new_value) =
                latch[chan].update(faders.get_value_at(chan), LatchLayer::Main, target)
            {
                storage.modify_and_save(|s| s.fader_saved[chan] = new_value);
            }
        }
    };

    // A tap cycles that channel's curve. On the two output channels it waits for the
    // release first, so a long press (handled below) can mute instead. Shift +
    // button 0 is the manual gate, read directly in the main loop.
    let button_handler = async {
        loop {
            let (chan, shift) = buttons.wait_for_any_down().await;
            if shift {
                continue;
            }
            if chan == OUT_CHAN || chan == ASR_CHAN {
                long_press_fired.set(false);
                buttons.wait_for_up(chan).await;
                if long_press_fired.get() {
                    continue;
                }
            }
            storage.modify_and_save(|s| {
                if let Some(curve) = s.curve_saved.get_mut(chan) {
                    *curve = curve.cycle();
                }
            });
        }
    };

    let long_press = async {
        loop {
            let (chan, shift) = buttons.wait_for_any_long_press().await;
            if shift {
                continue;
            }
            // Long press the ADSR output's channel to mute the ADSR, the last
            // channel to mute the ASR.
            let idx = if chan == OUT_CHAN {
                0
            } else if chan == ASR_CHAN {
                1
            } else {
                continue;
            };
            long_press_fired.set(true);
            storage.modify_and_save(|s| s.muted[idx] = !s.muted[idx]);
        }
    };

    let midi_handler = async {
        if midi_in.is_none() {
            core::future::pending::<()>().await;
        }
        let mut midi_input = app.use_midi_input(midi_in, midi_chan);
        loop {
            match midi_input.wait_for_message().await {
                MidiMessage::NoteOn { vel, .. } if vel.as_int() > 0 => {
                    notes_held.modify(|n| n.saturating_add(1));
                }
                MidiMessage::NoteOn { .. } | MidiMessage::NoteOff { .. } => {
                    notes_held.modify(|n| (n - 1).max(0));
                }
                _ => {}
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

    join(
        long_press,
        join5(
            main_loop,
            fader_handler,
            button_handler,
            midi_handler,
            scene_handler,
        ),
    )
    .await;
}
