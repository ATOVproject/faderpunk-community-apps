use embassy_futures::select::select;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use heapless::Vec;
use libfp::{AppIcon, Brightness, Color, Config, Curve, Range, Value, APP_MAX_PARAMS};

use crate::app::{App, AppParams, Led};

pub const CHANNELS: usize = 8;
pub const PARAMS: usize = 0;

pub static CONFIG: Config<PARAMS> = Config::new(
    "MATHS 2",
    "Dual independent function generators with bipolar attenuverters",
    Color::Cyan,
    AppIcon::AdEnv,
);

// The FPApp builder requires an AppParams implementation even when the app
// declares zero Configurator parameters. This empty type satisfies that
// contract and confirms that no parameter values are expected or stored.
pub struct Params;

impl AppParams for Params {
    fn from_values(values: &[Value]) -> Option<Self> {
        if values.is_empty() { Some(Self) } else { None }
    }

    fn to_values(&self) -> Vec<Value, APP_MAX_PARAMS> {
        Vec::new()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Rise,
    Fall,
}

struct Generator {
    phase: Phase,
    level: f32,
    segment_progress: f32,
    cycle: bool,
    previous_gate: bool,
    previous_trigger_button: bool,
    previous_cycle_button: bool,
    eor_flash_ms: u16,
    eoc_flash_ms: u16,
}

impl Generator {
    const fn new() -> Self {
        Self {
            phase: Phase::Idle,
            level: 0.0,
            segment_progress: 0.0,
            cycle: false,
            previous_gate: false,
            previous_trigger_button: false,
            previous_cycle_button: false,
            eor_flash_ms: 0,
            eoc_flash_ms: 0,
        }
    }

    fn trigger(&mut self) {
        self.phase = Phase::Rise;
        self.level = 0.0;
        self.segment_progress = 0.0;
    }
}

fn fader_time_ms(value: u16) -> f32 {
    // Exponential time range: 2 ms at the bottom, 10 s at the top.
    let x = value.min(4095) as f32 / 4095.0;
    2.0 * libm::powf(5000.0, x)
}

fn response_curve(progress: f32, response: u16) -> f32 {
    let p = (progress.clamp(0.0, 1.0) * 4095.0) as u16;
    let linear = p as f32 / 4095.0;
    let log = Curve::Logarithmic.at(p) as f32 / 4095.0;
    let exp = Curve::Exponential.at(p) as f32 / 4095.0;
    let r = response.min(4095) as f32 / 4095.0;
    if r < 0.5 {
        let mix = r * 2.0;
        log + (linear - log) * mix
    } else {
        let mix = (r - 0.5) * 2.0;
        linear + (exp - linear) * mix
    }
}

fn cv_time_scale(cv: u16) -> f32 {
    // 5 V is nominal; the CV input sweeps the segment time from 0.25x to 1.75x.
    let normalized = cv.min(4095) as f32 / 4095.0;
    0.25 + normalized * 1.5
}

fn step_generator(
    generator: &mut Generator,
    gate: u16,
    trigger_button: bool,
    cycle_button: bool,
    rise_ms: f32,
    fall_ms: f32,
    response: u16,
) {
    let gate_high = gate >= 410; // approximately 1 V on a 0-10 V input
    if gate_high && !generator.previous_gate {
        generator.trigger();
    }
    if trigger_button && !generator.previous_trigger_button {
        generator.trigger();
    }
    if cycle_button && !generator.previous_cycle_button {
        generator.cycle = !generator.cycle;
    }
    generator.previous_gate = gate_high;
    generator.previous_trigger_button = trigger_button;
    generator.previous_cycle_button = cycle_button;

    generator.eor_flash_ms = generator.eor_flash_ms.saturating_sub(1);
    generator.eoc_flash_ms = generator.eoc_flash_ms.saturating_sub(1);
    match generator.phase {
        Phase::Idle => {
            if generator.cycle {
                generator.trigger();
            }
        }
        Phase::Rise => {
            generator.segment_progress =
                (generator.segment_progress + 1.0 / rise_ms.max(1.0)).clamp(0.0, 1.0);
            generator.level = response_curve(generator.segment_progress, response);
            if generator.segment_progress >= 1.0 {
                generator.level = 1.0;
                generator.segment_progress = 0.0;
                generator.phase = Phase::Fall;
                generator.eor_flash_ms = 80;
            }
        }
        Phase::Fall => {
            generator.segment_progress =
                (generator.segment_progress + 1.0 / fall_ms.max(1.0)).clamp(0.0, 1.0);
            generator.level = 1.0 - response_curve(generator.segment_progress, response);
            if generator.segment_progress >= 1.0 {
                generator.level = 0.0;
                generator.segment_progress = 0.0;
                generator.phase = Phase::Idle;
                generator.eoc_flash_ms = 80;
                if generator.cycle {
                    generator.trigger();
                }
            }
        }
    }
}

fn bipolar_output(level: f32, attenuverter: u16) -> u16 {
    // A dead-zone curve guarantees a true zero around the fader centre.
    let centered = Curve::Deadzone.at(attenuverter.min(4095)) as i32 - 2048;
    let gain = centered as f32 / 2047.0;
    // The physical jack is configured for -5..+5 V. Full fader travel is
    // unity gain, so a full envelope reaches the hardware's +/-5 V limit.
    (2048.0 + level.clamp(0.0, 1.0) * gain * 2047.0)
        .round()
        .clamp(0.0, 4095.0) as u16
}

pub async fn wrapper(app: App<CHANNELS>, exit_signal: &'static Signal<NoopRawMutex, bool>) {
    let app_loop = async { run(&app).await };
    select(app_loop, app.exit_handler(exit_signal)).await;
}

pub async fn run(app: &App<CHANNELS>) {
    let faders = app.use_faders();
    let buttons = app.use_buttons();
    let leds = app.use_leds();

    // Physical channels 1-4: MATHS CH 1; 5-8: MATHS CH 2.
    let ch1_signal = app.make_in_jack(0, Range::_0_10V).await;
    let ch1_rise_cv = app.make_in_jack(1, Range::_0_10V).await;
    let ch1_fall_cv = app.make_in_jack(2, Range::_0_10V).await;
    let ch1_output = app.make_out_jack(3, Range::_Neg5_5V).await;

    let ch2_signal = app.make_in_jack(4, Range::_0_10V).await;
    let ch2_rise_cv = app.make_in_jack(5, Range::_0_10V).await;
    let ch2_fall_cv = app.make_in_jack(6, Range::_0_10V).await;
    let ch2_output = app.make_out_jack(7, Range::_Neg5_5V).await;

    let mut ch1 = Generator::new();
    let mut ch2 = Generator::new();

    leds.set(3, Led::Button, Color::Cyan, Brightness::Mid);
    leds.set(7, Led::Button, Color::Pink, Brightness::Mid);

    loop {
        app.delay_millis(1).await;

        let f = faders.get_all_values();
        let ch1_rise = fader_time_ms(f[0]) * cv_time_scale(ch1_rise_cv.get_value());
        let ch1_fall = fader_time_ms(f[1]) * cv_time_scale(ch1_fall_cv.get_value());
        let ch2_rise = fader_time_ms(f[4]) * cv_time_scale(ch2_rise_cv.get_value());
        let ch2_fall = fader_time_ms(f[5]) * cv_time_scale(ch2_fall_cv.get_value());

        let ch1_cycle_before = ch1.cycle;
        let ch2_cycle_before = ch2.cycle;
        step_generator(
            &mut ch1,
            ch1_signal.get_value(),
            buttons.is_button_pressed(0),
            buttons.is_button_pressed(3),
            ch1_rise,
            ch1_fall,
            f[2],
        );
        step_generator(
            &mut ch2,
            ch2_signal.get_value(),
            buttons.is_button_pressed(4),
            buttons.is_button_pressed(7),
            ch2_rise,
            ch2_fall,
            f[6],
        );

        ch1_output.set_value(bipolar_output(ch1.level, f[3]));
        ch2_output.set_value(bipolar_output(ch2.level, f[7]));

        // Fader LEDs show their live control positions. Output-channel LEDs
        // flash briefly for EOR/EOC; these are indications, not extra jacks.
        for (chan, value, color) in [
            (0, f[0], Color::Cyan),
            (1, f[1], Color::Cyan),
            (2, f[2], Color::Cyan),
            (4, f[4], Color::Pink),
            (5, f[5], Color::Pink),
            (6, f[6], Color::Pink),
        ] {
            leds.set(chan, Led::Top, color, Brightness::Custom((value / 16) as u8));
        }

        // Button LEDs change only when Cycle is toggled; output LEDs show
        // short EOR/EOC event flashes.
        if ch1.cycle != ch1_cycle_before {
            leds.set(3, Led::Button, if ch1.cycle { Color::Lime } else { Color::Cyan }, Brightness::Mid);
        }
        if ch2.cycle != ch2_cycle_before {
            leds.set(7, Led::Button, if ch2.cycle { Color::Lime } else { Color::Pink }, Brightness::Mid);
        }
        leds.set(
            3,
            Led::Top,
            Color::Cyan,
            if ch1.eor_flash_ms > 0 { Brightness::High } else { Brightness::Off },
        );
        leds.set(
            3,
            Led::Bottom,
            Color::Yellow,
            if ch1.eoc_flash_ms > 0 { Brightness::High } else { Brightness::Off },
        );
        leds.set(
            7,
            Led::Top,
            Color::Pink,
            if ch2.eor_flash_ms > 0 { Brightness::High } else { Brightness::Off },
        );
        leds.set(
            7,
            Led::Bottom,
            Color::Orange,
            if ch2.eoc_flash_ms > 0 { Brightness::High } else { Brightness::Off },
        );
    }
}
