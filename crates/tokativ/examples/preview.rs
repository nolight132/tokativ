//! Plays a scripted session in the real pill and caption, for the README picture.
//!
//! `cargo run -p tokativ --example preview`
//!
//! With `TOKATIV_PREVIEW_SHOT` set, its command runs through `sh -c` at each named moment with
//! the moment's name as `$1`. The moments while the last words animate in run in the background,
//! the `settled` one runs to completion before the pill and caption fold away, and the `closing`
//! ones follow while they do. `TOKATIV_PREVIEW_THEME` picks the theme by id, such as `dark`, and
//! `TOKATIV_PREVIEW_BUTTONS` lists the buttons to show, such as `confirm` or nothing at all.

#[path = "../src/assets.rs"]
mod assets;

use std::f32::consts::TAU;
use std::process::Command;
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, QuitMode};
use tokativ_audio::{BANDS, Levels};
use tokativ_config::Pill as PillConfig;

use assets::Assets;

/// What the caption shows at each step, after this long from the start.
const SCRIPT: &[(u64, &str, &str)] = &[
    (500, "", "The meeting"),
    (900, "", "The meeting moved to Thursday"),
    (1500, "The meeting moved to Thursday at three.", ""),
    (
        2100,
        "The meeting moved to Thursday at three.",
        "Let's sync after",
    ),
    (
        2700,
        "The meeting moved to Thursday at three.",
        "Let's sync after the demo and",
    ),
    (
        3400,
        "The meeting moved to Thursday at three. Let's sync after the demo and",
        "ship it on Friday",
    ),
];

/// The moments to capture after the last step, while its words come in.
const MOMENTS: &[u64] = &[40, 80, 120, 160, 200, 260, 340];
/// The moments to capture while the pill and caption fold away.
const CLOSING: &[u64] = &[30, 60, 90, 120];
const SETTLE: Duration = Duration::from_millis(2500);

fn main() {
    let application = gpui_platform::application().with_assets(Assets);
    application.run(|cx: &mut App| {
        cx.set_quit_mode(QuitMode::Explicit);
        Assets::load_fonts(cx).expect("loading the fonts");
        let theme = std::env::var("TOKATIV_PREVIEW_THEME").unwrap_or_else(|_| "system".into());
        tokativ_ui::themes::init(&theme, cx);
        let mut config = PillConfig::default();
        if let Ok(buttons) = std::env::var("TOKATIV_PREVIEW_BUTTONS") {
            config.cancel_button = buttons.contains("cancel");
            config.confirm_button = buttons.contains("confirm");
        }

        let levels = Levels::default();
        let pill = tokativ_ui::pill::open(
            levels.clone(),
            &config,
            |cx| cx.quit(),
            |cx| cx.quit(),
            false,
            cx,
        )
        .expect("opening the pill");
        let caption = tokativ_ui::caption::open(&config, None, cx)
            .expect("opening the caption");

        let started = Instant::now();
        cx.spawn(async move |cx: &mut AsyncApp| {
            loop {
                levels.show(speech(started.elapsed().as_secs_f32()));
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
            }
        })
        .detach();

        cx.spawn(async move |cx: &mut AsyncApp| {
            for (at, written, draft) in SCRIPT {
                let wait = Duration::from_millis(*at).saturating_sub(started.elapsed());
                cx.background_executor().timer(wait).await;
                caption
                    .update(cx, |caption, _, cx| caption.show(written, draft, cx))
                    .ok();
            }
            let last = Instant::now();
            for moment in MOMENTS {
                let wait = Duration::from_millis(*moment).saturating_sub(last.elapsed());
                cx.background_executor().timer(wait).await;
                shoot(&format!("moment-{moment:03}"), false);
            }
            cx.background_executor().timer(SETTLE).await;
            shoot("settled", true);
            caption
                .update(cx, |caption, _, cx| caption.dismiss(cx))
                .ok();
            pill.update(cx, |pill, _, cx| pill.dismiss(cx)).ok();
            let closed = Instant::now();
            for moment in CLOSING {
                let wait = Duration::from_millis(*moment).saturating_sub(closed.elapsed());
                cx.background_executor().timer(wait).await;
                shoot(&format!("closing-{moment:03}"), false);
            }
            cx.background_executor()
                .timer(Duration::from_millis(800))
                .await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

/// Bars that rise and fall like a voice, loudest in the low middle bands.
fn speech(time: f32) -> [f32; BANDS] {
    let syllables = (0.5 + 0.5 * (time * TAU * 2.3).sin()) * (0.6 + 0.4 * (time * TAU * 0.7).sin());
    std::array::from_fn(|band| {
        let place = band as f32 / (BANDS - 1) as f32;
        let shape = (-((place - 0.35) / 0.28).powi(2)).exp();
        let flicker = 0.85 + 0.15 * (time * TAU * 5.1 + band as f32 * 1.7).sin();
        (shape * (0.45 + 0.55 * syllables) * flicker).clamp(0., 1.)
    })
}

/// Runs the shot command for `moment`, if there is one.
fn shoot(moment: &str, wait: bool) {
    let Ok(command) = std::env::var("TOKATIV_PREVIEW_SHOT") else {
        return;
    };
    let mut child = match Command::new("sh")
        .args(["-c", &command, "shot", moment])
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!("preview: cannot run the shot command: {error}");
            return;
        }
    };
    if wait {
        child.wait().ok();
    }
}
