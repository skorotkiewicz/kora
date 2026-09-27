mod config;
mod desktop;
mod explorer;
mod operations;

use std::{cell::Cell, process::ExitCode, rc::Rc};

use desktop::Backend;
use gtk::prelude::*;
use gtk4 as gtk;

fn main() -> ExitCode {
    let explicit_config = match config::command(std::env::args_os()) {
        Ok(config::Command::Run(path)) => path,
        Ok(config::Command::Help) => {
            println!(
                "Kora {}\nDesktop folder browser for Niri and X11.\n\nUsage: kora [--config PATH]\n\n  --config PATH  Read this TOML file instead of the default\n  -h, --help     Show this help\n  -V, --version  Show the version\n\nDefault config: $XDG_CONFIG_HOME/kora/config.toml\nFallback: $HOME/.config/kora/config.toml",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        Ok(config::Command::Version) => {
            println!("kora {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
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
    // Remain available for output reconnection even when no view is mapped.
    let _desktop_hold = app.hold();
    let operations = operations::OperationQueue::new(&app);

    app.connect_activate({
        let config = config.clone();
        let operations = operations.clone();
        let failed = failed.clone();
        let initialized = Cell::new(false);
        move |app| {
            // A second process activates the primary GApplication; it must not
            // create another set of surfaces or monitor subscriptions.
            if initialized.replace(true) {
                return;
            }
            let result = gtk::gdk::Display::default()
                .ok_or_else(|| "no display is available".to_string())
                .and_then(|display| desktop::detect_backend(&display))
                .and_then(|backend| match backend {
                    Backend::Wayland => {
                        desktop::create_wayland_views(app, config.clone(), operations.clone())
                    }
                    Backend::X11 => {
                        desktop::create_x11_views(app, config.clone(), operations.clone())
                    }
                });

            if let Err(error) = result {
                desktop::show_startup_error(&error);
                failed.set(true);
                app.quit();
            }
        }
    });

    let exit = app.run_with_args::<&str>(&[]);
    if failed.get() {
        ExitCode::FAILURE
    } else {
        exit.into()
    }
}
