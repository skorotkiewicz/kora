mod config;
mod desktop;

use std::{cell::Cell, process::ExitCode, rc::Rc};

use desktop::Backend;
use gtk::prelude::*;
use gtk4 as gtk;

fn main() -> ExitCode {
    let explicit_config = match config::config_argument(std::env::args_os()) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("kora: {error}");
            return ExitCode::FAILURE;
        }
    };
    let config = match config::load(explicit_config) {
        Ok(config) => Rc::new(config),
        Err(error) => {
            eprintln!("kora: {error}");
            return ExitCode::FAILURE;
        }
    };
    let failed = Rc::new(Cell::new(false));
    let app = gtk::Application::builder()
        .application_id("io.github.kora")
        .build();

    app.connect_activate({
        let config = config.clone();
        let failed = failed.clone();
        move |app| {
            let result = gtk::gdk::Display::default()
                .ok_or_else(|| "no display is available".to_string())
                .and_then(|display| desktop::detect_backend(&display))
                .and_then(|backend| match backend {
                    Backend::Wayland => desktop::create_wayland_views(app, config.clone()),
                    Backend::X11 => desktop::create_x11_views(app, config.clone(), true),
                });

            if let Err(error) = result {
                desktop::show_startup_error(&error);
                failed.set(true);
                app.quit();
            }
        }
    });

    app.run_with_args::<&str>(&[]);
    if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
