use gtk::{gdk, glib, prelude::*};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConfigureWindowAux, ConnectionExt as _, PropMode, StackMode},
    wrapper::ConnectionExt as _,
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

fn validate_wayland_capabilities(supported: bool, version: u32) -> Result<(), String> {
    if !supported {
        return Err("the Wayland compositor does not support layer-shell".into());
    }
    if version < 4 {
        return Err("the Wayland compositor does not support on-demand layer-shell focus".into());
    }
    Ok(())
}

pub fn create_wayland_view(app: &gtk::Application) -> Result<gtk::ApplicationWindow, String> {
    validate_wayland_capabilities(
        gtk4_layer_shell::is_supported(),
        gtk4_layer_shell::protocol_version(),
    )?;

    let display = gdk::Display::default().ok_or("no display is available")?;
    let monitor = display
        .monitors()
        .item(0)
        .and_downcast::<gdk::Monitor>()
        .ok_or("no monitor is available")?;
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Kora")
        .decorated(false)
        .focusable(true)
        .build();

    window.init_layer_shell();
    window.set_namespace(Some("kora"));
    window.set_layer(Layer::Bottom);
    window.set_monitor(Some(&monitor));
    for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
        window.set_anchor(edge, true);
    }
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::None);

    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_top(16)
        .margin_start(16)
        .build();
    panel.append(&gtk::Label::new(Some("Kora")));
    let click = gtk::GestureClick::new();
    click.connect_pressed({
        let window = window.clone();
        move |_, _, _, _| {
            window.set_keyboard_mode(KeyboardMode::OnDemand);
            window.present();
        }
    });
    panel.add_controller(click);
    window.set_child(Some(&panel));
    window.set_visible(true);

    Ok(window)
}

fn atom(connection: &impl Connection, name: &str) -> Result<u32, String> {
    connection
        .intern_atom(false, name.as_bytes())
        .map_err(|error| error.to_string())?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|error| error.to_string())
}

pub fn create_x11_view(
    app: &gtk::Application,
    require_compositor: bool,
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

    if require_compositor {
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

    let display = gdk::Display::default().ok_or("no display is available")?;
    let monitor = display
        .monitors()
        .item(0)
        .and_downcast::<gdk::Monitor>()
        .ok_or("no monitor is available")?;
    let geometry = monitor.geometry();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Kora")
        .decorated(false)
        .focusable(true)
        .default_width(geometry.width())
        .default_height(geometry.height())
        .build();
    window.set_child(Some(&gtk::Label::new(Some("Kora"))));
    gtk::prelude::WidgetExt::realize(&window);

    let surface = window
        .surface()
        .and_downcast::<gdk4_x11::X11Surface>()
        .ok_or("GTK did not create an X11 surface")?;
    let xid = u32::try_from(surface.xid()).map_err(|_| "invalid X11 window id")?;
    let window_type = atom(&connection, "_NET_WM_WINDOW_TYPE")?;
    let desktop_type = atom(&connection, "_NET_WM_WINDOW_TYPE_DESKTOP")?;
    let window_state = atom(&connection, "_NET_WM_STATE")?;
    let wm_desktop = atom(&connection, "_NET_WM_DESKTOP")?;
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

    Ok(window)
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
