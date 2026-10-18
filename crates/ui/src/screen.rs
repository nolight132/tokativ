//! The user's main monitor, which the pill's placement is drawn to scale on. On Wayland GPUI
//! knows no primary output and rounds fractional scales, so the outputs are read here: Plasma
//! lists them in order with the primary first, and xdg-output gives their logical size.

use gpui::{App, AppContext as _, Pixels, Size, Task, px, size};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Screen {
    /// The resolution in device pixels.
    pub resolution: Size<u32>,
    /// The size in logical pixels, the unit the pill's distance from the edge is set in.
    pub size: Size<Pixels>,
}

impl Screen {
    /// What a monitor is most likely to be when none can be read.
    pub const FALLBACK: Self = Self {
        resolution: Size {
            width: 1920,
            height: 1080,
        },
        size: Size {
            width: px(1920.),
            height: px(1080.),
        },
    };
}

/// Reads the main monitor, on a thread of its own where that takes a connection.
pub(crate) fn primary(cx: &App) -> Task<Screen> {
    // GPUI's displays come in logical pixels, and the device pixels behind them stay unknown.
    let display = cx
        .primary_display()
        .or_else(|| cx.displays().into_iter().next())
        .map(|display| {
            let bounds = display.bounds().size;
            Screen {
                resolution: size(bounds.width.as_f32() as u32, bounds.height.as_f32() as u32),
                size: bounds,
            }
        })
        .unwrap_or(Screen::FALLBACK);
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return cx.background_spawn(async move {
            wayland::primary()
                .inspect_err(|error| log::info!("screen: cannot list the outputs: {error:#}"))
                .unwrap_or(display)
        });
    }
    Task::ready(display)
}

#[cfg(target_os = "linux")]
mod wayland {
    use anyhow::{Context as _, Result};
    use gpui::{px, size};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::wl_output::{self, WlOutput};
    use wayland_client::protocol::wl_registry::WlRegistry;
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
    use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
    use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};
    use wayland_protocols_plasma::output_order::v1::client::kde_output_order_v1::{
        self, KdeOutputOrderV1,
    };

    use super::Screen;

    #[derive(Default)]
    struct Output {
        name: Option<String>,
        mode: Option<(i32, i32)>,
        scale: i32,
        logical: Option<(i32, i32)>,
    }

    #[derive(Default)]
    struct State {
        outputs: Vec<Output>,
        /// Output names as Plasma orders them, the primary first.
        order: Vec<String>,
    }

    pub(super) fn primary() -> Result<Screen> {
        let connection = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
        let handle = queue.handle();
        let mut state = State::default();
        let listed: Vec<(u32, u32)> = globals.contents().with_list(|list| {
            list.iter()
                .filter(|global| global.interface == WlOutput::interface().name)
                .map(|global| (global.name, global.version))
                .collect()
        });
        let outputs: Vec<WlOutput> = listed
            .into_iter()
            .enumerate()
            .map(|(index, (name, version))| {
                state.outputs.push(Output {
                    scale: 1,
                    ..Output::default()
                });
                globals
                    .registry()
                    .bind(name, version.min(4), &handle, index)
            })
            .collect();
        if let Ok(manager) = globals.bind::<ZxdgOutputManagerV1, _, _>(&handle, 2..=3, ()) {
            for (index, output) in outputs.iter().enumerate() {
                manager.get_xdg_output(output, &handle, index);
            }
        }
        globals
            .bind::<KdeOutputOrderV1, _, _>(&handle, 1..=1, ())
            .ok();
        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;

        let ordered = state.order.first().and_then(|first| {
            state
                .outputs
                .iter()
                .find(|output| output.name.as_ref() == Some(first))
        });
        let output = ordered
            .or_else(|| state.outputs.iter().find(|output| output.mode.is_some()))
            .context("no output")?;
        let (mut width, mut height) = output.mode.context("the output has no mode")?;
        let (logical_width, logical_height) = output
            .logical
            .unwrap_or((width / output.scale.max(1), height / output.scale.max(1)));
        // The mode is before rotation and the logical size after it.
        if (width > height) != (logical_width > logical_height) {
            std::mem::swap(&mut width, &mut height);
        }
        Ok(Screen {
            resolution: size(width.max(1) as u32, height.max(1) as u32),
            size: size(
                px(logical_width.max(1) as f32),
                px(logical_height.max(1) as f32),
            ),
        })
    }

    impl Dispatch<WlOutput, usize> for State {
        fn event(
            state: &mut Self,
            _: &WlOutput,
            event: wl_output::Event,
            index: &usize,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let Some(output) = state.outputs.get_mut(*index) else {
                return;
            };
            match event {
                wl_output::Event::Mode {
                    flags: WEnum::Value(flags),
                    width,
                    height,
                    ..
                } if flags.contains(wl_output::Mode::Current) => {
                    output.mode = Some((width, height))
                }
                wl_output::Event::Scale { factor } => output.scale = factor,
                wl_output::Event::Name { name } => output.name = Some(name),
                _ => {}
            }
        }
    }

    impl Dispatch<ZxdgOutputV1, usize> for State {
        fn event(
            state: &mut Self,
            _: &ZxdgOutputV1,
            event: zxdg_output_v1::Event,
            index: &usize,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let Some(output) = state.outputs.get_mut(*index) else {
                return;
            };
            match event {
                zxdg_output_v1::Event::LogicalSize { width, height } => {
                    output.logical = Some((width, height))
                }
                zxdg_output_v1::Event::Name { name } if output.name.is_none() => {
                    output.name = Some(name)
                }
                _ => {}
            }
        }
    }

    impl Dispatch<KdeOutputOrderV1, ()> for State {
        fn event(
            state: &mut Self,
            _: &KdeOutputOrderV1,
            event: kde_output_order_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let kde_output_order_v1::Event::Output { output_name } = event {
                state.order.push(output_name);
            }
        }
    }

    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    wayland_client::delegate_noop!(State: ZxdgOutputManagerV1);
}
