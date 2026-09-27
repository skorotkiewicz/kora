use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use gtk::{gio, glib, prelude::*};
use gtk4 as gtk;

use crate::operations::OperationQueue;

#[cfg(test)]
use std::{fs, path::Path};

struct Explorer {
    current: RefCell<gio::File>,
    home: gio::File,
    back: RefCell<Vec<gio::File>>,
    forward: RefCell<Vec<gio::File>>,
    navigation_generation: Cell<u64>,
    directory: gtk::DirectoryList,
    selection: gtk::MultiSelection,
    operations: Rc<OperationQueue>,
    path_entry: gtk::Entry,
    error_label: gtk::Label,
    recovery: gtk::Box,
    back_button: gtk::Button,
    forward_button: gtk::Button,
}

pub fn view(start: PathBuf, home: PathBuf, operations: Rc<OperationQueue>) -> gtk::Box {
    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_top(16)
        .margin_start(16)
        .spacing(6)
        .width_request(640)
        .height_request(480)
        .build();
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let back_button = gtk::Button::with_label("Back");
    let forward_button = gtk::Button::with_label("Forward");
    let parent_button = gtk::Button::with_label("Parent");
    let home_button = gtk::Button::with_label("Home");
    let path_entry = gtk::Entry::new();
    path_entry.set_hexpand(true);
    path_entry.set_accessible_role(gtk::AccessibleRole::TextBox);
    let hidden_toggle = gtk::CheckButton::with_label("Hidden");
    for widget in [
        back_button.clone().upcast::<gtk::Widget>(),
        forward_button.clone().upcast(),
        parent_button.clone().upcast(),
        home_button.clone().upcast(),
        path_entry.clone().upcast(),
        hidden_toggle.clone().upcast(),
    ] {
        toolbar.append(&widget);
    }

    let error_label = gtk::Label::new(None);
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    error_label.add_css_class("error");
    let operation_label = gtk::Label::new(None);
    operation_label.set_xalign(0.0);
    operation_label.set_wrap(true);
    operation_label.set_visible(false);
    operations.subscribe(&operation_label);
    let recovery = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let retry_button = gtk::Button::with_label("Retry");
    let recovery_home_button = gtk::Button::with_label("Home");
    recovery.append(&retry_button);
    recovery.append(&recovery_home_button);

    let directory = gtk::DirectoryList::new(
        Some(
            "standard::name,standard::display-name,standard::type,standard::icon,standard::is-hidden,standard::is-symlink",
        ),
        None::<&gio::File>,
    );
    directory.set_monitored(true);
    let show_hidden = Rc::new(Cell::new(false));
    let filter = gtk::CustomFilter::new({
        let show_hidden = show_hidden.clone();
        move |object| {
            show_hidden.get()
                || !object
                    .downcast_ref::<gio::FileInfo>()
                    .is_some_and(gio::FileInfo::is_hidden)
        }
    });
    let filtered = gtk::FilterListModel::new(Some(directory.clone()), Some(filter.clone()));
    let sorter = gtk::CustomSorter::new(|left, right| {
        let left = left.downcast_ref::<gio::FileInfo>().unwrap();
        let right = right.downcast_ref::<gio::FileInfo>().unwrap();
        let left_directory = left.file_type() == gio::FileType::Directory;
        let right_directory = right.file_type() == gio::FileType::Directory;
        let ordering = right_directory.cmp(&left_directory).then_with(|| {
            left.display_name()
                .to_lowercase()
                .cmp(&right.display_name().to_lowercase())
        });
        match ordering {
            std::cmp::Ordering::Less => gtk::Ordering::Smaller,
            std::cmp::Ordering::Equal => gtk::Ordering::Equal,
            std::cmp::Ordering::Greater => gtk::Ordering::Larger,
        }
    });
    let sorted = gtk::SortListModel::new(Some(filtered), Some(sorter));
    let selection = gtk::MultiSelection::new(Some(sorted));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let tile = gtk::Box::new(gtk::Orientation::Vertical, 4);
        tile.set_size_request(96, 80);
        tile.set_accessible_role(gtk::AccessibleRole::ListItem);
        tile.append(&gtk::Image::new());
        let label = gtk::Label::new(None);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(14);
        tile.append(&label);
        item.set_child(Some(&tile));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let info = item.item().and_downcast::<gio::FileInfo>().unwrap();
        let tile = item.child().and_downcast::<gtk::Box>().unwrap();
        let image = tile.first_child().and_downcast::<gtk::Image>().unwrap();
        let label = tile.last_child().and_downcast::<gtk::Label>().unwrap();
        if let Some(icon) = info.icon() {
            image.set_from_gicon(&icon);
        } else {
            image.clear();
        }
        label.set_label(&info.display_name());
        tile.update_property(&[gtk::accessible::Property::Label(&info.display_name())]);
    });
    let grid = gtk::GridView::new(Some(selection.clone()), Some(factory));
    grid.set_min_columns(1);
    grid.set_max_columns(8);
    grid.set_enable_rubberband(true);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&grid)
        .build();

    panel.append(&toolbar);
    panel.append(&error_label);
    panel.append(&operation_label);
    panel.append(&recovery);
    panel.append(&scroller);

    let explorer = Rc::new(Explorer {
        current: RefCell::new(gio::File::for_path(&start)),
        home: gio::File::for_path(home),
        back: RefCell::new(Vec::new()),
        forward: RefCell::new(Vec::new()),
        navigation_generation: Cell::new(0),
        directory,
        selection: selection.clone(),
        operations,
        path_entry,
        error_label,
        recovery,
        back_button,
        forward_button,
    });
    explorer.navigate(gio::File::for_path(start), false);

    explorer
        .back_button
        .connect_clicked(with_explorer(&explorer, |explorer| explorer.go_back()));
    explorer
        .forward_button
        .connect_clicked(with_explorer(&explorer, |explorer| explorer.go_forward()));
    parent_button.connect_clicked(with_explorer(&explorer, |explorer| {
        if let Some(parent) = explorer.current.borrow().parent() {
            explorer.navigate(parent, true);
        }
    }));
    home_button.connect_clicked(with_explorer(&explorer, |explorer| {
        explorer.navigate(explorer.home.clone(), true)
    }));
    retry_button.connect_clicked(with_explorer(&explorer, |explorer| {
        explorer.navigate(explorer.current.borrow().clone(), false)
    }));
    recovery_home_button.connect_clicked(with_explorer(&explorer, |explorer| {
        explorer.navigate(explorer.home.clone(), true)
    }));
    explorer.path_entry.connect_activate({
        let explorer = Rc::downgrade(&explorer);
        move |entry| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.navigate(gio::File::for_path(entry.text()), true);
            }
        }
    });
    hidden_toggle.connect_toggled(move |toggle| {
        show_hidden.set(toggle.is_active());
        filter.changed(gtk::FilterChange::Different);
    });
    grid.connect_activate({
        let explorer = Rc::downgrade(&explorer);
        move |_, position| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.activate(position);
            }
        }
    });
    let popover = gtk::Popover::new();
    popover.set_parent(&grid);
    popover.set_accessible_role(gtk::AccessibleRole::Menu);
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let open = gtk::Button::with_label("Open");
    open.set_accessible_role(gtk::AccessibleRole::MenuItem);
    menu.append(&open);
    popover.set_child(Some(&menu));
    open.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.clone();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                let selected = explorer.selection.selection();
                if selected.size() > 0 {
                    explorer.activate(selected.minimum());
                }
            }
            popover.popdown();
        }
    });
    let context_click = gtk::GestureClick::new();
    context_click.set_button(3);
    context_click.connect_pressed({
        let popover = popover.clone();
        move |_, _, x, y| {
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        }
    });
    grid.add_controller(context_click);
    explorer.directory.connect_error_notify({
        let explorer = Rc::downgrade(&explorer);
        move |directory| {
            if let (Some(explorer), Some(error)) = (explorer.upgrade(), directory.error()) {
                explorer.show_error(&error.to_string());
            }
        }
    });
    panel.connect_destroy({
        let explorer = explorer.clone();
        move |_| {
            let _ = &explorer;
        }
    });

    panel
}

fn with_explorer<F>(explorer: &Rc<Explorer>, action: F) -> impl Fn(&gtk::Button) + 'static
where
    F: Fn(&Rc<Explorer>) + 'static,
{
    let explorer = Rc::downgrade(explorer);
    move |_| {
        if let Some(explorer) = explorer.upgrade() {
            action(&explorer);
        }
    }
}

impl Explorer {
    fn navigate(self: &Rc<Self>, target: gio::File, save_history: bool) {
        let Some(path) = target.path() else {
            self.show_error("only local folders are supported");
            return;
        };
        self.path_entry.set_text(&path.display().to_string());
        let generation = self.navigation_generation.get().wrapping_add(1);
        self.navigation_generation.set(generation);
        let explorer = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = target
                .enumerate_children_future(
                    "standard::name",
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::DEFAULT,
                )
                .await;
            let Some(explorer) = explorer.upgrade() else {
                return;
            };
            if explorer.navigation_generation.get() != generation {
                return;
            }
            match result {
                Ok(_) => {
                    if save_history {
                        explorer
                            .back
                            .borrow_mut()
                            .push(explorer.current.borrow().clone());
                        explorer.forward.borrow_mut().clear();
                    }
                    *explorer.current.borrow_mut() = target.clone();
                    explorer.directory.set_file(Some(&target));
                    explorer.error_label.set_visible(false);
                    explorer.recovery.set_visible(false);
                    explorer.update_history_buttons();
                }
                Err(error) => explorer.show_error(&error.to_string()),
            }
        });
    }

    fn go_back(self: &Rc<Self>) {
        let Some(target) = self.back.borrow_mut().pop() else {
            return;
        };
        self.forward
            .borrow_mut()
            .push(self.current.borrow().clone());
        self.navigate(target, false);
    }

    fn go_forward(self: &Rc<Self>) {
        let Some(target) = self.forward.borrow_mut().pop() else {
            return;
        };
        self.back.borrow_mut().push(self.current.borrow().clone());
        self.navigate(target, false);
    }

    fn activate(self: &Rc<Self>, position: u32) {
        let Some(info) = self
            .selection
            .item(position)
            .and_downcast::<gio::FileInfo>()
        else {
            return;
        };
        let child = self.current.borrow().child(info.name());
        let file_type =
            child.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE);
        if file_type == gio::FileType::Directory {
            self.navigate(child, true);
        } else if info.is_symlink() && file_type == gio::FileType::Unknown {
            self.show_error("broken symbolic link");
        } else {
            self.launch(&child);
        }
    }

    fn launch(&self, file: &gio::File) {
        if let Err(error) =
            gio::AppInfo::launch_default_for_uri(&file.uri(), gio::AppLaunchContext::NONE)
        {
            self.show_error(&format!("cannot open {}: {error}", file.parse_name()));
        }
    }

    fn show_error(&self, error: &str) {
        let path = self.path_entry.text();
        let message = format!("Folder error: {path}: {error}");
        glib::g_warning!("kora", "{message}");
        self.error_label.set_label(&message);
        self.error_label.set_visible(true);
        self.recovery.set_visible(true);
    }

    fn update_history_buttons(&self) {
        self.back_button
            .set_sensitive(!self.back.borrow().is_empty());
        self.forward_button
            .set_sensitive(!self.forward.borrow().is_empty());
    }
}

#[cfg(test)]
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
    fn file_uri_preserves_non_utf8_name() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let path = PathBuf::from(OsString::from_vec(b"/tmp/kora-\xff.txt".to_vec()));
        let uri = gio::File::for_path(path).uri();
        assert!(uri.contains("kora-%FF.txt"));
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
