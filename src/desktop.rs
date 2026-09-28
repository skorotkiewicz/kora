use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gtk::{gdk, glib, prelude::*};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use x11rb::{
    connection::Connection,
    protocol::xproto::{
        AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, InputFocus,
        PropMode, StackMode,
    },
    wrapper::ConnectionExt as _,
};

use crate::{
    config::{Config, Settings, WallpaperMode},
    explorer,
    operations::OperationQueue,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Wayland,
    X11,
}

pub fn detect_backend(display: &gdk::Display) -> Result<Backend, String> {
    match display.backend() {
        gdk::Backend::Wayland => Ok(Backend::Wayland),
        gdk::Backend::X11 => Ok(Backend::X11),
        backend => Err(format!("unsupported display backend: {backend:?}")),
    }
}

fn monitor_key(monitor: &gdk::Monitor, index: u32) -> String {
    monitor.connector().map_or_else(
        || {
            let geometry = monitor.geometry();
            glib::g_warning!(
                "kora",
                "monitor {index} has no connector name; using its geometry"
            );
            format!(
                "unnamed-{},{},{}x{}",
                geometry.x(),
                geometry.y(),
                geometry.width(),
                geometry.height()
            )
        },
        |connector| connector.to_string(),
    )
}

fn install_safe_close(window: &gtk::ApplicationWindow, operations: &Rc<OperationQueue>) {
    window.connect_close_request({
        let operations = operations.clone();
        move |window| {
            if !operations.is_active() {
                return glib::Propagation::Proceed;
            }
            window.set_visible(false);
            let weak = window.downgrade();
            operations.when_idle(move || {
                if let Some(window) = weak.upgrade() {
                    window.close();
                }
            });
            glib::Propagation::Stop
        }
    });
}

fn make_window_transparent(window: &gtk::ApplicationWindow) {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        ".kora-window, .kora-desktop, .kora-desktop scrolledwindow, \
         .kora-desktop viewport, .kora-desktop gridview { background-color: transparent; }\n\
         .kora-desktop gridview > child,\n\
         .kora-desktop gridview > child:hover,\n\
         .kora-desktop gridview > child:selected,\n\
         .kora-desktop gridview > child:focus { background-color: transparent;\n\
             background-image: none; box-shadow: none; outline: none; padding: 4px; }\n\
         .kora-file image { padding: 6px; border-radius: 8px;\n\
             transition: background-color 120ms ease-out; }\n\
         .kora-file label { color: white; border-radius: 4px; padding: 2px 4px;\n\
             text-shadow: 1px 1px 2px black, 0 0 3px black;\n\
             transition: background-color 120ms ease-out; }\n\
         .kora-desktop gridview > child:selected .kora-file image {\n\
             background-color: rgba(0,0,0,0.18);\n\
             box-shadow: inset 0 0 0 1px rgba(255,255,255,0.55); }\n\
         .kora-desktop gridview > child:selected .kora-file label {\n\
             background-color: #2367b5; text-shadow: none; }\n\
         .kora-desktop gridview > child:focus-visible .kora-file image {\n\
             outline: 2px solid white; outline-offset: 2px;\n\
             box-shadow: 0 0 0 3px rgba(0,0,0,0.55); }\n\
         .kora-menu button { min-height: 24px; padding: 2px 10px; }",
    );
    gtk::style_context_add_provider_for_display(
        &gtk::prelude::WidgetExt::display(window),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    window.add_css_class("kora-window");
}

fn build_view(
    settings: &Settings,
    home: &std::path::Path,
    operations: Rc<OperationQueue>,
) -> gtk::Overlay {
    let root = gtk::Overlay::new();
    let background = gtk::DrawingArea::new();
    if settings.wallpaper_mode == WallpaperMode::Replace {
        let red =
            u8::from_str_radix(&settings.background_color[1..3], 16).unwrap_or(32) as f64 / 255.0;
        let green =
            u8::from_str_radix(&settings.background_color[3..5], 16).unwrap_or(32) as f64 / 255.0;
        let blue =
            u8::from_str_radix(&settings.background_color[5..7], 16).unwrap_or(32) as f64 / 255.0;
        background.set_draw_func(move |_, context, width, height| {
            context.set_source_rgb(red, green, blue);
            context.rectangle(0.0, 0.0, width.into(), height.into());
            let _ = context.fill();
        });
    }
    root.set_child(Some(&background));

    let view = explorer::view(
        settings.path.clone(),
        home.to_path_buf(),
        settings.icon_size,
        operations,
    );

    if settings.wallpaper_mode == WallpaperMode::Replace
        && let Some(path) = &settings.wallpaper_image
    {
        match gdk::Texture::from_file(&gtk::gio::File::for_path(path)) {
            Ok(texture) => {
                let picture = gtk::Picture::for_paintable(&texture);
                picture.set_content_fit(gtk::ContentFit::Cover);
                picture.set_can_shrink(true);
                picture.set_hexpand(true);
                picture.set_vexpand(true);
                root.add_overlay(&picture);
            }
            Err(error) => {
                let message = format!("Wallpaper error: {}: {error}", path.display());
                glib::g_warning!("kora", "{message}");
                view.notifications.append(&gtk::Label::new(Some(&message)));
            }
        }
    }
    root.add_overlay(&view.root);
    root
}

fn validate_wayland_capabilities(supported: bool, version: u32) -> Result<(), String> {
    if !supported {
        return Err("the Wayland compositor does not support layer-shell".into());
    }
    if version < 4 {
        return Err("the Wayland compositor does not support on-demand layer-shell focus".into());
    }
    Ok(())
}

fn create_wayland_view(
    app: &gtk::Application,
    monitor: &gdk::Monitor,
    config: &Config,
    operations: Rc<OperationQueue>,
) -> Result<gtk::ApplicationWindow, String> {
    let settings = config.effective(monitor.connector().as_deref());
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Kora")
        .decorated(false)
        .focusable(true)
        .build();

    make_window_transparent(&window);
    window.init_layer_shell();
    window.set_namespace(Some("kora"));
    window.set_layer(Layer::Bottom);
    window.set_monitor(Some(monitor));
    for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
        window.set_anchor(edge, true);
    }
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::OnDemand);

    install_safe_close(&window, &operations);
    let content = build_view(&settings, config.home(), operations);
    window.set_child(Some(&content));
    window.set_visible(true);

    Ok(window)
}

fn sync_wayland_views(
    app: &gtk::Application,
    monitors: &gtk::gio::ListModel,
    views: &Rc<RefCell<HashMap<String, gtk::ApplicationWindow>>>,
    config: &Config,
    operations: Rc<OperationQueue>,
) -> Result<(), String> {
    let mut seen = Vec::new();
    for index in 0..monitors.n_items() {
        let monitor = monitors
            .item(index)
            .and_downcast::<gdk::Monitor>()
            .ok_or("display returned an invalid monitor")?;
        let key = monitor_key(&monitor, index);
        seen.push(key.clone());
        if !views.borrow().contains_key(&key) {
            let window = create_wayland_view(app, &monitor, config, operations.clone())?;
            views.borrow_mut().insert(key, window);
        }
    }
    views.borrow_mut().retain(|key, window| {
        let keep = seen.contains(key);
        if !keep {
            window.close();
        }
        keep
    });
    Ok(())
}

pub fn create_wayland_views(
    app: &gtk::Application,
    config: Rc<Config>,
    operations: Rc<OperationQueue>,
) -> Result<(), String> {
    validate_wayland_capabilities(
        gtk4_layer_shell::is_supported(),
        gtk4_layer_shell::protocol_version(),
    )?;
    let display = gdk::Display::default().ok_or("no display is available")?;
    let monitors = display.monitors();
    let views = Rc::new(RefCell::new(HashMap::new()));
    sync_wayland_views(app, &monitors, &views, &config, operations.clone())?;
    monitors.connect_items_changed({
        let app = app.clone();
        let monitors = monitors.clone();
        let views = views.clone();
        move |_, _, _, _| {
            if let Err(error) =
                sync_wayland_views(&app, &monitors, &views, &config, operations.clone())
            {
                show_startup_error(&error);
            }
        }
    });
    Ok(())
}

fn atom(connection: &impl Connection, name: &str) -> Result<u32, String> {
    connection
        .intern_atom(false, name.as_bytes())
        .map_err(|error| error.to_string())?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|error| error.to_string())
}

fn create_x11_view(
    app: &gtk::Application,
    monitor: &gdk::Monitor,
    config: &Config,
    operations: Rc<OperationQueue>,
) -> Result<gtk::ApplicationWindow, String> {
    let (connection, screen_index) = x11rb::connect(None).map_err(|error| error.to_string())?;
    let root = connection.setup().roots[screen_index].root;
    let net_supported = atom(&connection, "_NET_SUPPORTED")?;
    let required = [
        "_NET_WM_WINDOW_TYPE",
        "_NET_WM_WINDOW_TYPE_DESKTOP",
        "_NET_WM_STATE",
        "_NET_WM_STATE_BELOW",
        "_NET_WM_STATE_SKIP_TASKBAR",
        "_NET_WM_STATE_SKIP_PAGER",
        "_NET_WM_DESKTOP",
    ]
    .map(|name| atom(&connection, name))
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    let supported = connection
        .get_property(false, root, net_supported, AtomEnum::ATOM, 0, u32::MAX)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?
        .value32()
        .ok_or("the X11 window manager has an invalid _NET_SUPPORTED property")?
        .collect::<Vec<_>>();
    if required.iter().any(|atom| !supported.contains(atom)) {
        return Err(
            "the X11 window manager does not provide the required EWMH desktop-window support"
                .into(),
        );
    }

    let settings = config.effective(monitor.connector().as_deref());
    if settings.wallpaper_mode == WallpaperMode::Transparent {
        let selection = atom(&connection, &format!("_NET_WM_CM_S{screen_index}"))?;
        let owner = connection
            .get_selection_owner(selection)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| error.to_string())?
            .owner;
        if owner == x11rb::NONE {
            return Err("transparent mode on X11 requires a compositing manager".into());
        }
    }

    let geometry = monitor.geometry();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(format!("Kora: {}", settings.path.display()))
        .decorated(false)
        .focusable(true)
        .default_width(geometry.width())
        .default_height(geometry.height())
        .build();
    make_window_transparent(&window);
    install_safe_close(&window, &operations);
    let content = build_view(&settings, config.home(), operations);
    window.set_child(Some(&content));
    gtk::prelude::WidgetExt::realize(&window);

    let surface = window
        .surface()
        .and_downcast::<gdk4_x11::X11Surface>()
        .ok_or("GTK did not create an X11 surface")?;
    let xid = u32::try_from(surface.xid()).map_err(|_| "invalid X11 window id")?;
    let focus = gtk::EventControllerLegacy::builder()
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    focus.connect_event(move |_, event| {
        if event.event_type() == gdk::EventType::ButtonPress
            && let Ok((connection, _)) = x11rb::connect(None)
        {
            let _ = connection.set_input_focus(InputFocus::PARENT, xid, x11rb::CURRENT_TIME);
            let _ = connection.flush();
        }
        glib::Propagation::Proceed
    });
    content.add_controller(focus);
    let window_type = atom(&connection, "_NET_WM_WINDOW_TYPE")?;
    let desktop_type = atom(&connection, "_NET_WM_WINDOW_TYPE_DESKTOP")?;
    let window_state = atom(&connection, "_NET_WM_STATE")?;
    let wm_desktop = atom(&connection, "_NET_WM_DESKTOP")?;
    connection
        .change_window_attributes(xid, &ChangeWindowAttributesAux::new().override_redirect(1))
        .map_err(|error| error.to_string())?;
    let states = [
        atom(&connection, "_NET_WM_STATE_BELOW")?,
        atom(&connection, "_NET_WM_STATE_SKIP_TASKBAR")?,
        atom(&connection, "_NET_WM_STATE_SKIP_PAGER")?,
        atom(&connection, "_NET_WM_STATE_STICKY")?,
    ];
    connection
        .change_property32(
            PropMode::REPLACE,
            xid,
            window_type,
            AtomEnum::ATOM,
            &[desktop_type],
        )
        .and_then(|_| {
            connection.change_property32(
                PropMode::REPLACE,
                xid,
                window_state,
                AtomEnum::ATOM,
                &states,
            )
        })
        .and_then(|_| {
            connection.change_property32(
                PropMode::REPLACE,
                xid,
                wm_desktop,
                AtomEnum::CARDINAL,
                &[u32::MAX],
            )
        })
        .map_err(|error| error.to_string())?;
    connection
        .configure_window(
            xid,
            &ConfigureWindowAux::new()
                .x(geometry.x())
                .y(geometry.y())
                .width(geometry.width() as u32)
                .height(geometry.height() as u32)
                .stack_mode(StackMode::BELOW),
        )
        .map_err(|error| error.to_string())?;
    connection.flush().map_err(|error| error.to_string())?;
    window.set_visible(true);
    monitor.connect_notify_local(Some("geometry"), move |monitor, _| {
        let Ok((connection, _)) = x11rb::connect(None) else {
            return;
        };
        let geometry = monitor.geometry();
        let _ = connection.configure_window(
            xid,
            &ConfigureWindowAux::new()
                .x(geometry.x())
                .y(geometry.y())
                .width(geometry.width() as u32)
                .height(geometry.height() as u32),
        );
        let _ = connection.flush();
    });

    Ok(window)
}

fn sync_x11_views(
    app: &gtk::Application,
    monitors: &gtk::gio::ListModel,
    views: &Rc<RefCell<HashMap<String, gtk::ApplicationWindow>>>,
    config: &Config,
    operations: Rc<OperationQueue>,
) -> Result<(), String> {
    let mut seen = Vec::new();
    for index in 0..monitors.n_items() {
        let monitor = monitors
            .item(index)
            .and_downcast::<gdk::Monitor>()
            .ok_or("display returned an invalid monitor")?;
        let key = monitor_key(&monitor, index);
        seen.push(key.clone());
        if !views.borrow().contains_key(&key) {
            let window = create_x11_view(app, &monitor, config, operations.clone())?;
            views.borrow_mut().insert(key, window);
        }
    }
    views.borrow_mut().retain(|key, window| {
        let keep = seen.contains(key);
        if !keep {
            window.close();
        }
        keep
    });
    Ok(())
}

pub fn create_x11_views(
    app: &gtk::Application,
    config: Rc<Config>,
    operations: Rc<OperationQueue>,
) -> Result<(), String> {
    let display = gdk::Display::default().ok_or("no display is available")?;
    let monitors = display.monitors();
    let views = Rc::new(RefCell::new(HashMap::new()));
    sync_x11_views(app, &monitors, &views, &config, operations.clone())?;
    monitors.connect_items_changed({
        let app = app.clone();
        let monitors = monitors.clone();
        let views = views.clone();
        move |_, _, _, _| {
            if let Err(error) = sync_x11_views(&app, &monitors, &views, &config, operations.clone())
            {
                show_startup_error(&error);
            }
        }
    });
    Ok(())
}

pub fn show_startup_error(error: &str) {
    glib::g_printerr!("kora: {error}\n");
}

#[cfg(test)]
mod tests {
    use super::validate_wayland_capabilities;

    #[test]
    fn rejects_missing_layer_shell() {
        assert_eq!(
            validate_wayland_capabilities(false, 0).unwrap_err(),
            "the Wayland compositor does not support layer-shell"
        );
    }

    #[test]
    fn rejects_old_layer_shell() {
        assert_eq!(
            validate_wayland_capabilities(true, 3).unwrap_err(),
            "the Wayland compositor does not support on-demand layer-shell focus"
        );
    }
}
