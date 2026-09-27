mod desktop;

use std::{cell::Cell, process::ExitCode, rc::Rc};

use desktop::Backend;
use gtk::prelude::*;
use gtk4 as gtk;

fn main() -> ExitCode {
    let failed = Rc::new(Cell::new(false));
    let app = gtk::Application::builder()
        .application_id("io.github.kora")
        .build();

    app.connect_activate({
        let failed = failed.clone();
        move |app| {
            let result = gtk::gdk::Display::default()
                .ok_or_else(|| "no display is available".to_string())
                .and_then(|display| desktop::detect_backend(&display))
                .and_then(|backend| match backend {
                    Backend::Wayland => desktop::create_wayland_view(app).map(|_| ()),
                    Backend::X11 => Err("X11 desktop integration is not implemented yet".into()),
                });

            if let Err(error) = result {
                desktop::show_startup_error(&error);
                failed.set(true);
                app.quit();
            }
        }
    });

    app.run();
    if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
