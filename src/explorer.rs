use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use gtk::{gio, glib, prelude::*};
use gtk4 as gtk;

use crate::operations::{Operation, OperationQueue};

fn trash_operation(paths: Vec<PathBuf>, confirmed: bool) -> Option<Operation> {
    confirmed.then_some(Operation::Trash { paths })
}

fn local_drag_paths(files: &[gio::File]) -> Result<Vec<PathBuf>, String> {
    if files.is_empty() {
        return Err("the drag contains no files".into());
    }
    files
        .iter()
        .map(|file| {
            let path = file
                .path()
                .ok_or_else(|| format!("unsupported non-local URI: {}", file.uri()))?;
            let kind = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?
                .file_type();
            if kind.is_file() || kind.is_dir() || kind.is_symlink() {
                Ok(path)
            } else {
                Err(format!("unsupported special file: {}", path.display()))
            }
        })
        .collect()
}

fn drag_provider(paths: &[PathBuf]) -> gtk::gdk::ContentProvider {
    let files = paths.iter().map(gio::File::for_path).collect::<Vec<_>>();
    let file_list = gtk::gdk::FileList::from_array(&files);
    let native = gtk::gdk::ContentProvider::for_value(&file_list.to_value());
    let uri_list = files
        .iter()
        .map(gio::File::uri)
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n";
    let uri = gtk::gdk::ContentProvider::for_bytes(
        "text/uri-list",
        &glib::Bytes::from(uri_list.as_bytes()),
    );
    gtk::gdk::ContentProvider::new_union(&[native, uri])
}

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
    drop_label: gtk::Label,
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
    let drop_label = gtk::Label::new(Some("Drag status: idle"));
    drop_label.set_xalign(0.0);
    let current_drop_zone = gtk::Label::new(Some("Drop files here to current folder"));
    current_drop_zone.set_xalign(0.0);
    current_drop_zone.set_margin_top(4);
    current_drop_zone.set_margin_bottom(4);
    current_drop_zone.set_accessible_role(gtk::AccessibleRole::Group);
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
    let explorer_slot = Rc::new(RefCell::new(std::rc::Weak::<Explorer>::new()));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup({
        let explorer_slot = explorer_slot.clone();
        move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let tile = gtk::Box::new(gtk::Orientation::Vertical, 4);
            tile.set_size_request(96, 80);
            tile.set_accessible_role(gtk::AccessibleRole::ListItem);
            tile.append(&gtk::Image::new());
            let label = gtk::Label::new(None);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(14);
            tile.append(&label);
            let drop_target = gtk::DropTarget::new(
                gtk::gdk::FileList::static_type(),
                gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
            );
            drop_target.set_preload(true);
            drop_target.set_propagation_phase(gtk::PropagationPhase::Capture);
            drop_target.connect_motion({
                let item = item.clone();
                let explorer_slot = explorer_slot.clone();
                move |target, _, _| {
                    let Some(explorer) = explorer_slot.borrow().upgrade() else {
                        return gtk::gdk::DragAction::empty();
                    };
                    let Some(destination) = explorer.folder_for_item(&item) else {
                        return gtk::gdk::DragAction::empty();
                    };
                    let action = Explorer::drop_action(target);
                    explorer.show_drop_action(&destination, action);
                    action
                }
            });
            drop_target.connect_leave({
                let explorer_slot = explorer_slot.clone();
                move |_| {
                    if let Some(explorer) = explorer_slot.borrow().upgrade() {
                        explorer.drop_label.set_label("Drag status: idle");
                    }
                }
            });
            drop_target.connect_drop({
                let item = item.clone();
                let explorer_slot = explorer_slot.clone();
                move |target, value, _, _| {
                    let Some(explorer) = explorer_slot.borrow().upgrade() else {
                        return false;
                    };
                    let Some(destination) = explorer.folder_for_item(&item) else {
                        return false;
                    };
                    explorer.accept_drop(target, value, destination)
                }
            });
            tile.add_controller(drop_target);
            let drag_source = gtk::DragSource::new();
            drag_source.set_propagation_phase(gtk::PropagationPhase::Capture);
            drag_source.set_actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE);
            drag_source.connect_prepare({
                let explorer_slot = explorer_slot.clone();
                move |_, _, _| {
                    let explorer = explorer_slot.borrow().upgrade()?;
                    let paths = explorer.selected_paths();
                    if paths.is_empty() {
                        None
                    } else {
                        explorer
                            .drop_label
                            .set_label(&format!("Dragging {} item(s)", paths.len()));
                        Some(drag_provider(&paths))
                    }
                }
            });
            tile.add_controller(drag_source);
            item.set_child(Some(&tile));
        }
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
    panel.append(&drop_label);
    panel.append(&current_drop_zone);
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
        drop_label,
        recovery,
        back_button,
        forward_button,
    });
    *explorer_slot.borrow_mut() = Rc::downgrade(&explorer);

    let current_drop = gtk::DropTarget::new(
        gtk::gdk::FileList::static_type(),
        gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
    );
    current_drop.set_preload(true);
    current_drop.connect_motion({
        let explorer = Rc::downgrade(&explorer);
        move |target, _, _| {
            let Some(explorer) = explorer.upgrade() else {
                return gtk::gdk::DragAction::empty();
            };
            let Some(destination) = explorer.current.borrow().path() else {
                return gtk::gdk::DragAction::empty();
            };
            let action = Explorer::drop_action(target);
            explorer.show_drop_action(&destination, action);
            action
        }
    });
    current_drop.connect_leave({
        let explorer = Rc::downgrade(&explorer);
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.drop_label.set_label("Drag status: idle");
            }
        }
    });
    current_drop.connect_drop({
        let explorer = Rc::downgrade(&explorer);
        move |target, value, _, _| {
            let Some(explorer) = explorer.upgrade() else {
                return false;
            };
            let Some(destination) = explorer.current.borrow().path() else {
                return false;
            };
            explorer.accept_drop(target, value, destination)
        }
    });
    current_drop_zone.add_controller(current_drop);

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
    let rename = gtk::Button::with_label("Rename");
    let copy = gtk::Button::with_label("Copy");
    let cut = gtk::Button::with_label("Cut");
    let paste = gtk::Button::with_label("Paste");
    let trash = gtk::Button::with_label("Move to Trash");
    for button in [&open, &rename, &copy, &cut, &paste, &trash] {
        button.set_accessible_role(gtk::AccessibleRole::MenuItem);
        menu.append(button);
    }
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
    rename.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.clone();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.show_rename();
            }
            popover.popdown();
        }
    });
    copy.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.clone();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.copy_selected(false);
            }
            popover.popdown();
        }
    });
    cut.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.clone();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.copy_selected(true);
            }
            popover.popdown();
        }
    });
    paste.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.clone();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.paste();
            }
            popover.popdown();
        }
    });
    trash.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.clone();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.confirm_trash();
            }
            popover.popdown();
        }
    });
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed({
        let explorer = Rc::downgrade(&explorer);
        move |_, key, _, modifiers| {
            let Some(explorer) = explorer.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let handled = if key == gtk::gdk::Key::F2 {
                explorer.show_rename();
                true
            } else if key == gtk::gdk::Key::Delete {
                explorer.confirm_trash();
                true
            } else if control && key == gtk::gdk::Key::c {
                explorer.copy_selected(false);
                true
            } else if control && key == gtk::gdk::Key::x {
                explorer.copy_selected(true);
                true
            } else if control && key == gtk::gdk::Key::v {
                explorer.paste();
                true
            } else if control && key == gtk::gdk::Key::l {
                explorer.path_entry.grab_focus();
                true
            } else if control && key == gtk::gdk::Key::q {
                explorer.confirm_quit();
                true
            } else {
                false
            };
            handled.into()
        }
    });
    panel.add_controller(keys);
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

    fn folder_for_item(&self, item: &gtk::ListItem) -> Option<PathBuf> {
        let info = item.item().and_downcast::<gio::FileInfo>()?;
        let file = self.current.borrow().child(info.name());
        (file.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
            == gio::FileType::Directory)
            .then(|| file.path())
            .flatten()
    }

    fn drop_action(target: &gtk::DropTarget) -> gtk::gdk::DragAction {
        let Some(drop) = target.current_drop() else {
            return gtk::gdk::DragAction::empty();
        };
        let offered = drop.actions();
        let internal = drop.drag().is_some();
        let explicit_move = target
            .current_event_state()
            .contains(gtk::gdk::ModifierType::SHIFT_MASK);
        if internal && explicit_move && offered.contains(gtk::gdk::DragAction::MOVE) {
            gtk::gdk::DragAction::MOVE
        } else if offered.contains(gtk::gdk::DragAction::COPY) {
            gtk::gdk::DragAction::COPY
        } else if internal && offered.contains(gtk::gdk::DragAction::MOVE) {
            gtk::gdk::DragAction::MOVE
        } else {
            gtk::gdk::DragAction::empty()
        }
    }

    fn show_drop_action(&self, destination: &std::path::Path, action: gtk::gdk::DragAction) {
        if action.is_empty() {
            self.drop_label
                .set_label("Drop rejected: no safe file action is available");
        } else {
            let verb = if action == gtk::gdk::DragAction::MOVE {
                "Move"
            } else {
                "Copy"
            };
            self.drop_label
                .set_label(&format!("{verb} to {}", destination.display()));
        }
    }

    fn accept_drop(
        &self,
        target: &gtk::DropTarget,
        value: &glib::Value,
        destination: PathBuf,
    ) -> bool {
        let Ok(file_list) = value.get::<gtk::gdk::FileList>() else {
            self.show_operation_error("unsupported drag payload");
            return false;
        };
        let sources = match local_drag_paths(&file_list.files()) {
            Ok(paths) => paths,
            Err(error) => {
                self.show_operation_error(&error);
                return false;
            }
        };
        let action = Self::drop_action(target);
        let operation = if action == gtk::gdk::DragAction::MOVE {
            Operation::Move {
                sources,
                destination,
            }
        } else if action == gtk::gdk::DragAction::COPY {
            Operation::Copy {
                sources,
                destination,
            }
        } else {
            return false;
        };
        self.drop_label.set_label("Drag status: accepted");
        if let Err(error) = self.operations.enqueue(operation) {
            self.show_operation_error(&error);
            false
        } else {
            true
        }
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        let current = self.current.borrow();
        (0..self.selection.n_items())
            .filter(|position| self.selection.is_selected(*position))
            .filter_map(|position| {
                self.selection
                    .item(position)
                    .and_downcast::<gio::FileInfo>()
                    .and_then(|info| current.child(info.name()).path())
            })
            .collect()
    }

    fn copy_selected(&self, move_files: bool) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.show_operation_error("select at least one item");
        } else {
            self.operations.set_clipboard(paths, move_files);
        }
    }

    fn paste(&self) {
        let Some((move_files, sources)) = self.operations.clipboard() else {
            self.show_operation_error("nothing to paste");
            return;
        };
        let Some(destination) = self.current.borrow().path() else {
            self.show_operation_error("only local destinations are supported");
            return;
        };
        let operation = if move_files {
            Operation::Move {
                sources,
                destination,
            }
        } else {
            Operation::Copy {
                sources,
                destination,
            }
        };
        if let Err(error) = self.operations.enqueue(operation) {
            self.show_operation_error(&error);
        }
    }

    fn show_rename(self: &Rc<Self>) {
        let paths = self.selected_paths();
        if paths.len() != 1 {
            self.show_operation_error("select exactly one item to rename");
            return;
        }
        let source = paths[0].clone();
        let dialog = gtk::Dialog::builder().title("Rename").modal(true).build();
        if let Some(parent) = self.path_entry.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&parent));
        }
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Rename", gtk::ResponseType::Accept);
        let entry = gtk::Entry::new();
        entry.set_activates_default(true);
        entry.set_text(&source.file_name().unwrap_or_default().to_string_lossy());
        entry.set_accessible_role(gtk::AccessibleRole::TextBox);
        dialog.content_area().append(&entry);
        dialog.set_default_response(gtk::ResponseType::Accept);
        gtk::prelude::GtkWindowExt::set_focus(&dialog, Some(&entry));
        dialog.connect_response({
            let explorer = Rc::downgrade(self);
            move |dialog, response| {
                if response == gtk::ResponseType::Accept
                    && let Some(explorer) = explorer.upgrade()
                {
                    let operation = Operation::Rename {
                        source: source.clone(),
                        new_name: entry.text().as_str().into(),
                    };
                    if let Err(error) = explorer.operations.enqueue(operation) {
                        explorer.show_operation_error(&error);
                    }
                }
                dialog.close();
            }
        });
        dialog.present();
    }

    fn confirm_trash(self: &Rc<Self>) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.show_operation_error("select at least one item to move to Trash");
            return;
        }
        let parent = self.path_entry.root().and_downcast::<gtk::Window>();
        let dialog = gtk::MessageDialog::new(
            parent.as_ref(),
            gtk::DialogFlags::MODAL,
            gtk::MessageType::Question,
            gtk::ButtonsType::None,
            format!("Move {} selected item(s) to Trash?", paths.len()),
        );
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Move to Trash", gtk::ResponseType::Accept);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        dialog.connect_response({
            let explorer = Rc::downgrade(self);
            move |dialog, response| {
                if let Some(operation) =
                    trash_operation(paths.clone(), response == gtk::ResponseType::Accept)
                    && let Some(explorer) = explorer.upgrade()
                    && let Err(error) = explorer.operations.enqueue(operation)
                {
                    explorer.show_operation_error(&error);
                }
                dialog.close();
            }
        });
        dialog.present();
    }

    fn confirm_quit(&self) {
        let Some(parent) = self.path_entry.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let dialog = gtk::MessageDialog::new(
            Some(&parent),
            gtk::DialogFlags::MODAL,
            gtk::MessageType::Question,
            gtk::ButtonsType::None,
            "Quit Kora? Active file operations will finish before exit.",
        );
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Quit Safely", gtk::ResponseType::Accept);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                parent.close();
            }
            dialog.close();
        });
        dialog.present();
    }

    fn launch(&self, file: &gio::File) {
        if let Err(error) =
            gio::AppInfo::launch_default_for_uri(&file.uri(), gio::AppLaunchContext::NONE)
        {
            self.show_error(&format!("cannot open {}: {error}", file.parse_name()));
        }
    }

    fn show_operation_error(&self, error: &str) {
        let message = format!("File operation error: {error}");
        glib::g_warning!("kora", "{message}");
        self.error_label.set_label(&message);
        self.error_label.set_visible(true);
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
    fn cancelled_trash_confirmation_does_not_create_an_operation() {
        let directory = temp_path("cancel-trash");
        fs::create_dir(&directory).unwrap();
        let file = directory.join("file");
        fs::write(&file, "content").unwrap();
        assert!(trash_operation(vec![file.clone()], false).is_none());
        assert_eq!(fs::read_to_string(file).unwrap(), "content");
        fs::remove_dir_all(directory).unwrap();
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

    #[test]
    fn drag_uri_round_trip_preserves_escaped_names() {
        let directory = temp_path("drag-uri");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("name #%.txt");
        fs::write(&path, "content").unwrap();
        let uri = gio::File::for_path(&path).uri();
        assert!(uri.contains("name%20%23%25.txt"));
        assert_eq!(
            local_drag_paths(&[gio::File::for_uri(&uri)]).unwrap(),
            vec![path]
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_entire_mixed_or_missing_drag_payload() {
        let directory = temp_path("drag-invalid");
        fs::create_dir(&directory).unwrap();
        let existing = directory.join("existing");
        fs::write(&existing, "content").unwrap();
        assert!(
            local_drag_paths(&[
                gio::File::for_path(&existing),
                gio::File::for_uri("https://example.invalid/file"),
            ])
            .is_err()
        );
        assert!(
            local_drag_paths(&[
                gio::File::for_path(&existing),
                gio::File::for_path(directory.join("missing")),
            ])
            .is_err()
        );
        assert_eq!(fs::read_to_string(existing).unwrap(), "content");
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
