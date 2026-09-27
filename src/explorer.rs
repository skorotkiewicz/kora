use std::{cell::RefCell, fs, path::Path, path::PathBuf, rc::Rc};

use gtk::prelude::*;
use gtk4 as gtk;

pub fn start_panel(start: PathBuf, home: PathBuf) -> gtk::Box {
    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_top(16)
        .margin_start(16)
        .spacing(6)
        .build();
    let path_label = gtk::Label::new(None);
    path_label.set_xalign(0.0);
    let error_label = gtk::Label::new(None);
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let retry = gtk::Button::with_label("Retry");
    let go_home = gtk::Button::with_label("Home");
    actions.append(&retry);
    actions.append(&go_home);
    panel.append(&path_label);
    panel.append(&error_label);
    panel.append(&actions);

    let current = Rc::new(RefCell::new(start));
    retry.connect_clicked({
        let current = current.clone();
        let path_label = path_label.clone();
        let error_label = error_label.clone();
        let actions = actions.clone();
        move |_| refresh(&current.borrow(), &path_label, &error_label, &actions)
    });
    go_home.connect_clicked({
        let current = current.clone();
        let path_label = path_label.clone();
        let error_label = error_label.clone();
        let actions = actions.clone();
        move |_| {
            *current.borrow_mut() = home.clone();
            refresh(&current.borrow(), &path_label, &error_label, &actions);
        }
    });
    refresh(&current.borrow(), &path_label, &error_label, &actions);
    panel
}

fn refresh(path: &Path, path_label: &gtk::Label, error_label: &gtk::Label, actions: &gtk::Box) {
    path_label.set_label(&path.display().to_string());
    match validate_start_path(path) {
        Ok(()) => {
            error_label.set_visible(false);
            actions.set_visible(false);
        }
        Err(error) => {
            let message = format!("Folder error: {}: {error}", path.display());
            gtk::glib::g_warning!("kora", "{message}");
            error_label.set_label(&message);
            error_label.set_visible(true);
            actions.set_visible(true);
        }
    }
}

fn validate_start_path(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err("not a directory".into());
    }
    fs::read_dir(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "kora-explorer-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn accepts_directory_and_rejects_missing_path_and_file() {
        let directory = temp_path("directory");
        fs::create_dir(&directory).unwrap();
        assert_eq!(validate_start_path(&directory), Ok(()));
        assert!(validate_start_path(&directory.join("missing")).is_err());
        let file = directory.join("file");
        File::create(&file).unwrap();
        assert_eq!(validate_start_path(&file).unwrap_err(), "not a directory");
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_unreadable_directory() {
        use std::os::unix::fs::PermissionsExt;

        let directory = temp_path("unreadable");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o000)).unwrap();
        let result = validate_start_path(&directory);
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir(directory).unwrap();
        assert!(result.is_err());
    }
}
