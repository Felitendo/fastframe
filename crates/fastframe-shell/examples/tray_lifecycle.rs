//! Manual native lifecycle regression: hide/show and recreate several times,
//! opening the tray menu and choosing an action after every transition.

use fastframe_shell::{Closed, Headless, Held, Resident, Shell, Waker};
use fastframe_tray::{Config, Event, MenuItem, Tray};

struct App {
    tray: Tray,
    closing: Closed,
    show: bool,
    small: bool,
    generation: usize,
}

impl App {
    fn events(&mut self, ctx: Option<&egui::Context>) {
        for event in self.tray.events() {
            match event {
                Event::Show => self.show = true,
                Event::Toggle | Event::Menu("toggle") => {
                    if let Some(ctx) = ctx {
                        self.closing = Closed::Hide;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else {
                        self.show = true;
                    }
                }
                Event::Menu("recreate") => {
                    self.small = !self.small;
                    self.closing = Closed::Reopen;
                    if let Some(ctx) = ctx {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else {
                        self.show = true;
                    }
                }
                Event::Menu("quit") => {
                    self.closing = Closed::Quit;
                    if let Some(ctx) = ctx {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                Event::Menu(_) => {}
            }
        }
        if self.show
            && let Some(ctx) = ctx
        {
            self.show = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }
}

impl Resident for App {
    fn closed(&self) -> Closed {
        self.closing
    }
    fn window_gone(&mut self) {
        self.show = false;
    }
    fn start_hidden(&mut self) -> bool {
        self.tray.create_item();
        true
    }
    fn headless_frame(&mut self, _ctx: &egui::Context) -> Headless {
        self.events(None);
        if self.closing == Closed::Quit {
            Headless::Quit
        } else if self.show {
            Headless::Show
        } else {
            Headless::Wait
        }
    }
    fn shutdown(&mut self) {}
}

struct Window(Held<App>);
impl eframe::App for Window {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.0.events(Some(ctx));
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.heading("Tray lifecycle");
        ui.label(format!("Window {}", self.0.generation));
        ui.label("Close this window, then choose Show or hide from the tray.");
        if ui.button("Recreate the window").clicked() {
            self.0.small = !self.0.small;
            self.0.closing = Closed::Reopen;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn main() -> eframe::Result {
    let waker = Waker::default();
    let wake = waker.clone();
    let Some(tray) = Tray::spawn(
        Config {
            id: "fastframe-lifecycle",
            title: "Fastframe lifecycle".into(),
            icon: |size| [0x40, 0x90, 0xe0, 0xff].repeat(size * size),
            template_icon: None,
            themed_icon: false,
            menu: vec![
                MenuItem::action("toggle", "Show or hide"),
                MenuItem::action("recreate", "Recreate window"),
                MenuItem::action("quit", "Quit fixture"),
            ],
        },
        move || wake.wake(),
    ) else {
        return Ok(());
    };
    let app = App {
        tray,
        closing: Closed::Hide,
        show: false,
        small: false,
        generation: 0,
    };
    Shell::new(app, &waker)
        .idle(fastframe_tray::idle)
        .start_hidden(std::env::args().any(|arg| arg == "--hidden"))
        .run(|lease| {
            let size = lease.peek(|app| {
                if app.small {
                    [320., 160.]
                } else {
                    [520., 240.]
                }
            });
            let options = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default().with_inner_size(size),
                ..Default::default()
            };
            eframe::run_native(
                "Fastframe lifecycle",
                options,
                Box::new(move |cc| {
                    let mut app = lease.take(&cc.egui_ctx);
                    app.generation += 1;
                    app.closing = Closed::Hide;
                    app.show = false;
                    app.tray.attach();
                    Ok(Box::new(Window(app)))
                }),
            )
        })
}
