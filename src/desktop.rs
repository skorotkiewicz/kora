use gtk::{gdk, glib, prelude::*};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

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
