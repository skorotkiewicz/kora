use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use gtk::{gio, glib, prelude::*};
use gtk4 as gtk;

use crate::operations::{ExternalDragItem, Operation, OperationQueue};

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
    gtk::gdk::ContentProvider::new_union(&[uri, native])
}

#[cfg(test)]
use std::{fs, path::Path};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Navigation {
    Visit,
    Reload,
    Back,
    Forward,
}

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
}

pub struct View {
    pub root: gtk::Overlay,
    #[cfg(test)]
    pub grid: gtk::GridView,
    #[cfg(test)]
    pub input_widgets: Vec<gtk::Widget>,
    #[cfg(test)]
    pub scroller: gtk::ScrolledWindow,
    #[cfg(test)]
    explorer: Rc<Explorer>,
    pub notifications: gtk::Box,
}

pub fn view(start: PathBuf, home: PathBuf, icon_size: i32, operations: Rc<OperationQueue>) -> View {
    let root = gtk::Overlay::builder().hexpand(true).vexpand(true).build();
    root.add_css_class("kora-desktop");

    let controls = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .margin_end(16)
        .margin_bottom(16)
        .build();
    let back_button = gtk::Button::with_label("Back");
    back_button.set_tooltip_text(Some("Go back (Alt+Left)"));
    back_button.set_visible(false);
    let hidden_toggle = gtk::CheckButton::with_label("Show hidden files");
    hidden_toggle.set_tooltip_text(Some("Show hidden files (Ctrl+H)"));
    let folder_button = gtk::Button::from_icon_name("folder-open-symbolic");
    folder_button.set_tooltip_text(Some(
        "Current folder: click to enter a path, or drop files here",
    ));
    folder_button.update_property(&[gtk::accessible::Property::Label("Current folder")]);
    controls.append(&folder_button);
    controls.append(&back_button);
    controls.append(&hidden_toggle);

    let path_entry = gtk::Entry::new();
    path_entry.set_width_request(560);
    path_entry.set_accessible_role(gtk::AccessibleRole::TextBox);
    path_entry.set_visible(false);

    let error_label = gtk::Label::new(None);
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    error_label.set_visible(false);
    error_label.add_css_class("error");
    let operation_label = gtk::Label::new(None);
    operation_label.set_xalign(0.0);
    operation_label.set_wrap(true);
    operation_label.set_visible(false);
    operations.subscribe(&operation_label);
    let drop_label = gtk::Label::new(None);
    drop_label.set_xalign(0.0);
    drop_label.set_visible(false);
    let recovery = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    recovery.set_visible(false);
    let retry_button = gtk::Button::with_label("Retry");
    let recovery_home_button = gtk::Button::with_label("Home");
    recovery.append(&retry_button);
    recovery.append(&recovery_home_button);

    let notifications = gtk::Box::new(gtk::Orientation::Vertical, 6);
    notifications.set_halign(gtk::Align::Center);
    notifications.set_valign(gtk::Align::Start);
    notifications.set_margin_top(16);
    notifications.append(&path_entry);
    notifications.append(&error_label);
    notifications.append(&operation_label);
    notifications.append(&drop_label);
    notifications.append(&recovery);

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
    hidden_toggle.connect_toggled({
        let filter = filter.clone();
        move |toggle| {
            show_hidden.set(toggle.is_active());
            filter.changed(gtk::FilterChange::Different);
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
            tile.set_size_request((icon_size + 24).max(88), icon_size + 28);
            tile.add_css_class("kora-file");
            tile.set_accessible_role(gtk::AccessibleRole::ListItem);
            let image = gtk::Image::new();
            image.set_pixel_size(icon_size);
            image.set_halign(gtk::Align::Center);
            tile.append(&image);
            let label = gtk::Label::new(None);
            label.set_single_line_mode(true);
            label.set_justify(gtk::Justification::Center);
            label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            label.set_max_width_chars(10);
            label.set_halign(gtk::Align::Center);
            tile.append(&label);
            let context_selection = gtk::EventControllerLegacy::builder()
                .propagation_phase(gtk::PropagationPhase::Capture)
                .build();
            context_selection.connect_event({
                let item = item.downgrade();
                let explorer_slot = explorer_slot.clone();
                move |_, event| {
                    if event.event_type() == gtk::gdk::EventType::ButtonPress
                        && let Some(button) = event.downcast_ref::<gtk::gdk::ButtonEvent>()
                        && button.button() == 3
                        && let Some(item) = item.upgrade()
                        && let Some(explorer) = explorer_slot.borrow().upgrade()
                        && !explorer.selection.is_selected(item.position())
                    {
                        explorer.selection.select_item(item.position(), true);
                    }
                    glib::Propagation::Proceed
                }
            });
            tile.add_controller(context_selection);
            let drop_target = gtk::DropTarget::new(
                gtk::gdk::FileList::static_type(),
                gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
            );
            drop_target.set_preload(true);
            drop_target.set_propagation_phase(gtk::PropagationPhase::Capture);
            drop_target.connect_motion({
                let item = item.downgrade();
                let explorer_slot = explorer_slot.clone();
                move |target, _, _| {
                    let Some(explorer) = explorer_slot.borrow().upgrade() else {
                        return gtk::gdk::DragAction::empty();
                    };
                    let Some(item) = item.upgrade() else {
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
                        explorer.drop_label.set_visible(false);
                    }
                }
            });
            drop_target.connect_drop({
                let item = item.downgrade();
                let explorer_slot = explorer_slot.clone();
                move |target, value, _, _| {
                    let Some(explorer) = explorer_slot.borrow().upgrade() else {
                        return false;
                    };
                    let Some(item) = item.upgrade() else {
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
            let drag_snapshot =
                Rc::new(RefCell::new(None::<(Vec<PathBuf>, Vec<ExternalDragItem>)>));
            drag_source.set_propagation_phase(gtk::PropagationPhase::Capture);
            drag_source.set_actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE);
            drag_source.connect_prepare({
                let explorer_slot = explorer_slot.clone();
                let drag_snapshot = drag_snapshot.clone();
                let item = item.downgrade();
                move |_, _, _| {
                    let explorer = explorer_slot.borrow().upgrade()?;
                    let item = item.upgrade()?;
                    if !explorer.selection.is_selected(item.position()) {
                        explorer.selection.select_item(item.position(), true);
                    }
                    let paths = explorer.selected_paths();
                    if paths.is_empty() {
                        return None;
                    }
                    let snapshot = match explorer.operations.snapshot_external_drag(&paths) {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            explorer.show_operation_error(&error);
                            return None;
                        }
                    };
                    *drag_snapshot.borrow_mut() = Some((paths.clone(), snapshot));
                    explorer
                        .drop_label
                        .set_label(&format!("Dragging {} item(s)", paths.len()));
                    explorer.drop_label.set_visible(true);
                    Some(drag_provider(&paths))
                }
            });
            drag_source.connect_drag_end({
                let explorer_slot = explorer_slot.clone();
                let drag_snapshot = drag_snapshot.clone();
                move |_, _, delete_data| {
                    let Some((paths, items)) = drag_snapshot.borrow_mut().take() else {
                        return;
                    };
                    let Some(explorer) = explorer_slot.borrow().upgrade() else {
                        return;
                    };
                    explorer.drop_label.set_visible(false);
                    let internal_move = explorer.operations.consume_internal_drag_move(&paths);
                    if !delete_data || internal_move {
                        return;
                    }
                    if let Err(error) = explorer
                        .operations
                        .enqueue(Operation::FinishExternalMove { items })
                    {
                        explorer.show_operation_error(&error);
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
        tile.set_tooltip_text(Some(&info.display_name()));
        tile.update_property(&[gtk::accessible::Property::Label(&info.display_name())]);
    });
    let grid = gtk::GridView::new(Some(selection.clone()), Some(factory));
    grid.set_min_columns(1);
    grid.set_max_columns(32);
    grid.set_enable_rubberband(true);
    grid.set_margin_top(16);
    grid.set_margin_bottom(16);
    grid.set_margin_start(16);
    grid.set_margin_end(16);
    // Let the native scroller receive wheel and touchpad input over empty space too.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .kinetic_scrolling(true)
        .vexpand(true)
        .hexpand(true)
        .child(&grid)
        .build();
    root.set_child(Some(&scroller));
    root.add_overlay(&notifications);
    root.add_overlay(&controls);

    let explorer = Rc::new(Explorer {
        current: RefCell::new(gio::File::for_path(&start)),
        home: gio::File::for_path(home),
        back: RefCell::new(Vec::new()),
        forward: RefCell::new(Vec::new()),
        navigation_generation: Cell::new(0),
        directory,
        selection: selection.clone(),
        operations,
        path_entry: path_entry.clone(),
        error_label,
        drop_label,
        recovery: recovery.clone(),
        back_button: back_button.clone(),
    });
    *explorer_slot.borrow_mut() = Rc::downgrade(&explorer);

    let formats = gtk::gdk::ContentFormats::for_type(gtk::gdk::FileList::static_type());
    let current_drop = gtk::DropTargetAsync::new(
        Some(formats),
        gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
    );
    let current_drop_action = Rc::new(Cell::new(gtk::gdk::DragAction::empty()));
    current_drop.connect_drag_motion({
        let explorer = Rc::downgrade(&explorer);
        let current_drop_action = current_drop_action.clone();
        move |target, drop, _, _| {
            let Some(explorer) = explorer.upgrade() else {
                return gtk::gdk::DragAction::empty();
            };
            let Some(destination) = explorer.current.borrow().path() else {
                return gtk::gdk::DragAction::empty();
            };
            let action = Explorer::async_drop_action(target, drop);
            current_drop_action.set(action);
            explorer.show_drop_action(&destination, action);
            action
        }
    });
    current_drop.connect_drag_leave({
        let explorer = Rc::downgrade(&explorer);
        let current_drop_action = current_drop_action.clone();
        move |_, _| {
            current_drop_action.set(gtk::gdk::DragAction::empty());
            if let Some(explorer) = explorer.upgrade() {
                explorer.drop_label.set_visible(false);
            }
        }
    });
    current_drop.connect_drop({
        let explorer = Rc::downgrade(&explorer);
        let current_drop_action = current_drop_action.clone();
        move |_, drop, _, _| {
            let Some(explorer) = explorer.upgrade() else {
                return false;
            };
            let Some(destination) = explorer.current.borrow().path() else {
                return false;
            };
            let action = current_drop_action.replace(gtk::gdk::DragAction::empty());
            if action.is_empty() {
                return false;
            }
            let internal = drop.drag().is_some();
            let drop = drop.clone();
            let operations = explorer.operations.clone();
            let weak_explorer = Rc::downgrade(&explorer);
            drop.clone().read_value_async(
                gtk::gdk::FileList::static_type(),
                glib::Priority::DEFAULT,
                gio::Cancellable::NONE,
                move |value| {
                    let sources = value
                        .map_err(|error| error.to_string())
                        .and_then(|value| {
                            value
                                .get::<gtk::gdk::FileList>()
                                .map_err(|error| error.to_string())
                        })
                        .and_then(|files| local_drag_paths(&files.files()));
                    let sources = match sources {
                        Ok(sources) => sources,
                        Err(error) => {
                            if let Some(explorer) = weak_explorer.upgrade() {
                                explorer.show_operation_error(&error);
                            }
                            drop.finish(gtk::gdk::DragAction::empty());
                            return;
                        }
                    };
                    let operation = if action == gtk::gdk::DragAction::MOVE && internal {
                        Operation::Move {
                            sources: sources.clone(),
                            destination,
                        }
                    } else {
                        Operation::Copy {
                            sources: sources.clone(),
                            destination,
                        }
                    };
                    let finished_drop = drop.clone();
                    match operations.enqueue_with_callback(operation, move |success| {
                        finished_drop.finish(if success {
                            action
                        } else {
                            gtk::gdk::DragAction::empty()
                        });
                    }) {
                        Ok(_) if action == gtk::gdk::DragAction::MOVE && internal => {
                            operations.mark_internal_drag_move(sources);
                        }
                        Ok(_) => {}
                        Err(error) => {
                            if let Some(explorer) = weak_explorer.upgrade() {
                                explorer.show_operation_error(&error);
                            }
                            drop.finish(gtk::gdk::DragAction::empty());
                        }
                    }
                },
            );
            true
        }
    });
    folder_button.add_controller(current_drop);

    explorer.navigate(gio::File::for_path(start), Navigation::Reload);

    folder_button.connect_clicked(with_explorer(&explorer, |explorer| {
        explorer.path_entry.set_visible(true);
        explorer.path_entry.grab_focus();
    }));
    back_button.connect_clicked(with_explorer(&explorer, {
        let grid = grid.clone();
        move |explorer| {
            explorer.go_back();
            grid.grab_focus();
        }
    }));
    retry_button.connect_clicked(with_explorer(&explorer, |explorer| {
        explorer.navigate(
            gio::File::for_path(explorer.path_entry.text()),
            Navigation::Reload,
        )
    }));
    recovery_home_button.connect_clicked(with_explorer(&explorer, |explorer| {
        explorer.navigate(explorer.home.clone(), Navigation::Visit)
    }));
    explorer.path_entry.connect_activate({
        let explorer = Rc::downgrade(&explorer);
        move |entry| {
            if let Some(explorer) = explorer.upgrade() {
                entry.set_visible(false);
                explorer.navigate(gio::File::for_path(entry.text()), Navigation::Visit);
            }
        }
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
    popover.set_has_arrow(false);
    popover.add_css_class("kora-menu");
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let open = gtk::Button::with_label("Open");
    let open_with = gtk::Button::with_label("Open with…");
    let rename = gtk::Button::with_label("Rename");
    let copy = gtk::Button::with_label("Copy");
    let cut = gtk::Button::with_label("Cut");
    let paste = gtk::Button::with_label("Paste");
    let trash = gtk::Button::with_label("Move to Trash");
    let refresh = gtk::Button::with_label("Refresh");
    refresh.set_tooltip_text(Some("Reload the current folder (F5)"));
    let results = gtk::Button::with_label("Operation results");
    let quit = gtk::Button::with_label("Quit Kora");
    let more_menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let more_popover = gtk::Popover::new();
    more_popover.set_accessible_role(gtk::AccessibleRole::Menu);
    more_popover.set_has_arrow(false);
    more_popover.add_css_class("kora-menu");
    more_popover.set_child(Some(&more_menu));
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("More")
        .popover(&more_popover)
        .build();
    more.update_property(&[gtk::accessible::Property::Label("More")]);
    controls.append(&more);
    for button in [
        &open, &open_with, &rename, &copy, &cut, &paste, &trash, &refresh, &results, &quit,
    ] {
        button.set_accessible_role(gtk::AccessibleRole::MenuItem);
        button.add_css_class("flat");
        if let Some(label) = button.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.0);
        }
        if button == &copy || button == &refresh {
            menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        if button == &results || button == &quit {
            more_menu.append(button);
        } else {
            menu.append(button);
        }
    }
    popover.set_child(Some(&menu));
    refresh.connect_clicked(with_explorer(&explorer, {
        let popover = popover.downgrade();
        move |explorer| {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
            explorer.refresh();
        }
    }));
    results.connect_clicked(with_explorer(&explorer, {
        let popover = more_popover.downgrade();
        move |explorer| {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
            explorer.show_results();
        }
    }));
    quit.connect_clicked(with_explorer(&explorer, {
        let popover = more_popover.downgrade();
        move |explorer| {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
            explorer.confirm_quit();
        }
    }));
    open.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.downgrade();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                let selected = explorer.selection.selection();
                if selected.size() > 0 {
                    explorer.activate(selected.minimum());
                }
            }
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        }
    });
    open_with.connect_clicked(with_explorer(&explorer, {
        let popover = popover.downgrade();
        move |explorer| {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
            if let Some(dialog) = explorer.open_with_dialog() {
                dialog.present();
            }
        }
    }));
    rename.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.downgrade();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.show_rename();
            }
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        }
    });
    copy.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.downgrade();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.copy_selected(false);
            }
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        }
    });
    cut.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.downgrade();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.copy_selected(true);
            }
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        }
    });
    paste.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.downgrade();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.paste();
            }
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        }
    });
    trash.connect_clicked({
        let explorer = Rc::downgrade(&explorer);
        let popover = popover.downgrade();
        move |_| {
            if let Some(explorer) = explorer.upgrade() {
                explorer.confirm_trash();
            }
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        }
    });
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed({
        let explorer = Rc::downgrade(&explorer);
        let hidden_toggle = hidden_toggle.clone();
        let grid = grid.clone();
        move |_, key, _, modifiers| {
            let Some(explorer) = explorer.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let alt = modifiers.contains(gtk::gdk::ModifierType::ALT_MASK);
            let handled = if key == gtk::gdk::Key::Escape
                && gtk::prelude::WidgetExt::is_visible(&explorer.path_entry)
            {
                explorer.path_entry.set_visible(false);
                grid.grab_focus();
                true
            } else if key == gtk::gdk::Key::F5 {
                explorer.refresh();
                true
            } else if key == gtk::gdk::Key::F2 {
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
                explorer.path_entry.set_visible(true);
                explorer.path_entry.grab_focus();
                true
            } else if control && key == gtk::gdk::Key::h {
                hidden_toggle.set_active(!hidden_toggle.is_active());
                true
            } else if alt && key == gtk::gdk::Key::Left {
                explorer.go_back();
                true
            } else if alt && key == gtk::gdk::Key::Right {
                explorer.go_forward();
                true
            } else if alt && key == gtk::gdk::Key::Up {
                if let Some(parent) = explorer.current.borrow().parent() {
                    explorer.navigate(parent, Navigation::Visit);
                }
                true
            } else if alt && key == gtk::gdk::Key::Home {
                explorer.navigate(explorer.home.clone(), Navigation::Visit);
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
    root.add_controller(keys);
    let context_click = gtk::GestureClick::new();
    context_click.set_button(3);
    context_click.connect_pressed({
        let popover = popover.clone();
        let explorer = Rc::downgrade(&explorer);
        move |_, _, x, y| {
            let Some(explorer) = explorer.upgrade() else {
                return;
            };
            let selected = explorer.selection.selection().size();
            open.set_sensitive(selected > 0);
            open_with.set_sensitive(selected == 1);
            rename.set_sensitive(selected == 1);
            copy.set_sensitive(selected > 0);
            cut.set_sensitive(selected > 0);
            trash.set_sensitive(selected > 0);
            paste.set_sensitive(
                explorer
                    .operations
                    .clipboard()
                    .is_some_and(|(_, paths)| !paths.is_empty()),
            );
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
    root.connect_destroy({
        let explorer = explorer.clone();
        let popover = popover.clone();
        move |_| {
            popover.unparent();
            let _ = &explorer;
        }
    });

    View {
        root,
        #[cfg(test)]
        grid,
        #[cfg(test)]
        input_widgets: vec![
            path_entry.upcast(),
            recovery.upcast(),
            back_button.upcast(),
            hidden_toggle.upcast(),
            folder_button.upcast(),
            more.upcast(),
        ],
        #[cfg(test)]
        scroller,
        #[cfg(test)]
        explorer,
        notifications,
    }
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
    fn refresh(self: &Rc<Self>) {
        let current = self.current.borrow().clone();
        self.navigate(current, Navigation::Reload);
    }

    fn navigate(self: &Rc<Self>, target: gio::File, navigation: Navigation) {
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
                    let current = explorer.current.borrow().clone();
                    match navigation {
                        Navigation::Visit if !current.equal(&target) => {
                            explorer.back.borrow_mut().push(current);
                            explorer.forward.borrow_mut().clear();
                        }
                        Navigation::Back => {
                            explorer.back.borrow_mut().pop();
                            explorer.forward.borrow_mut().push(current);
                        }
                        Navigation::Forward => {
                            explorer.forward.borrow_mut().pop();
                            explorer.back.borrow_mut().push(current);
                        }
                        _ => {}
                    }
                    *explorer.current.borrow_mut() = target.clone();
                    if navigation == Navigation::Reload {
                        // Setting the same file is a no-op. Reset only after validation succeeds.
                        explorer.directory.set_file(gio::File::NONE);
                    }
                    explorer.directory.set_file(Some(&target));
                    explorer.error_label.set_visible(false);
                    explorer.recovery.set_visible(false);
                    explorer
                        .back_button
                        .set_visible(!explorer.back.borrow().is_empty());
                }
                Err(error) => explorer.show_error(&error.to_string()),
            }
        });
    }

    fn go_back(self: &Rc<Self>) {
        let Some(target) = self.back.borrow().last().cloned() else {
            return;
        };
        self.navigate(target, Navigation::Back);
    }

    fn go_forward(self: &Rc<Self>) {
        let Some(target) = self.forward.borrow().last().cloned() else {
            return;
        };
        self.navigate(target, Navigation::Forward);
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
            self.navigate(child, Navigation::Visit);
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

    fn async_drop_action(
        target: &gtk::DropTargetAsync,
        drop: &gtk::gdk::Drop,
    ) -> gtk::gdk::DragAction {
        let offered = drop.actions();
        let explicit_move = target
            .current_event_state()
            .contains(gtk::gdk::ModifierType::SHIFT_MASK);
        if explicit_move && offered.contains(gtk::gdk::DragAction::MOVE) {
            gtk::gdk::DragAction::MOVE
        } else if offered.contains(gtk::gdk::DragAction::COPY) {
            gtk::gdk::DragAction::COPY
        } else if offered.contains(gtk::gdk::DragAction::MOVE) {
            gtk::gdk::DragAction::MOVE
        } else {
            gtk::gdk::DragAction::empty()
        }
    }

    fn show_drop_action(&self, destination: &std::path::Path, action: gtk::gdk::DragAction) {
        self.drop_label.set_visible(true);
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
        let internal_move = action == gtk::gdk::DragAction::MOVE;
        let moved_sources = sources.clone();
        let operation = if internal_move {
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
            if internal_move {
                self.operations.mark_internal_drag_move(moved_sources);
            }
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

    fn show_results(&self) {
        let dialog = gtk::Dialog::builder()
            .title("Operation results")
            .default_width(600)
            .default_height(320)
            .build();
        if let Some(parent) = self.path_entry.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&parent));
        }
        dialog.add_button("Close", gtk::ResponseType::Close);
        let messages = self.operations.result_messages();
        let label = gtk::Label::new(Some(if messages.is_empty() {
            "No completed operations"
        } else {
            &messages
        }));
        label.set_wrap(true);
        label.set_selectable(true);
        label.set_xalign(0.0);
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&label)
            .build();
        dialog.content_area().append(&scroller);
        dialog.connect_response(|dialog, _| dialog.close());
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
        let operations = self.operations.clone();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response == gtk::ResponseType::Accept {
                operations.quit_when_idle();
            }
        });
        dialog.present();
    }

    fn open_with_dialog(self: &Rc<Self>) -> Option<gtk::AppChooserDialog> {
        let paths = self.selected_paths();
        let [path] = paths.as_slice() else {
            self.show_operation_error("select exactly one item to open with another application");
            return None;
        };
        let file = gio::File::for_path(path);
        let parent = self.path_entry.root().and_downcast::<gtk::Window>();
        let dialog = gtk::AppChooserDialog::new(
            parent.as_ref(),
            gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
            &file,
        );
        dialog.set_title(Some("Open with"));
        dialog.connect_response({
            let explorer = Rc::downgrade(self);
            move |dialog, response| {
                if response == gtk::ResponseType::Ok
                    && let Some(explorer) = explorer.upgrade()
                {
                    if let Some(app) = dialog.app_info() {
                        let context = gtk::prelude::WidgetExt::display(dialog).app_launch_context();
                        if let Err(error) = app.launch(std::slice::from_ref(&file), Some(&context))
                        {
                            explorer.show_operation_error(&format!(
                                "cannot open {} with {}: {error}",
                                file.parse_name(),
                                app.display_name(),
                            ));
                        }
                    } else {
                        explorer.show_operation_error("no application was selected");
                    }
                }
                dialog.close();
            }
        });
        Some(dialog)
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
    #[ignore = "requires a GTK display; does not open a window"]
    fn desktop_controls_filter_hidden_files_and_go_back() {
        gtk::init().unwrap();
        let app = gtk::Application::builder()
            .application_id("dev.kora.ControlsTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let view = view(root.clone(), root, 48, OperationQueue::new(&app));
        assert!(view.scroller.is_kinetic_scrolling());
        assert!(view.scroller.hexpands() && view.scroller.vexpands());
        assert_eq!(view.scroller.margin_top(), 0);
        assert_eq!(view.scroller.margin_bottom(), 0);
        assert_eq!(view.scroller.margin_start(), 0);
        assert_eq!(view.scroller.margin_end(), 0);
        let model = view.grid.model().unwrap();
        let back = view.input_widgets[2]
            .clone()
            .downcast::<gtk::Button>()
            .unwrap();
        let hidden = view.input_widgets[3]
            .clone()
            .downcast::<gtk::CheckButton>()
            .unwrap();
        let position = |name: &str| {
            (0..model.n_items()).find(|&index| {
                model
                    .item(index)
                    .and_downcast::<gio::FileInfo>()
                    .unwrap()
                    .name()
                    == Path::new(name)
            })
        };
        let wait = |condition: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !condition() {
                assert!(std::time::Instant::now() < deadline, "GTK update timed out");
                glib::MainContext::default().iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        wait(&|| position("Cargo.toml").is_some());
        assert!(!back.is_visible());
        assert!(position(".gitignore").is_none());
        hidden.set_active(true);
        wait(&|| position(".gitignore").is_some());
        hidden.set_active(false);
        wait(&|| position(".gitignore").is_none());
        view.grid
            .emit_by_name::<()>("activate", &[&position("src").unwrap()]);
        wait(&|| back.is_visible() && position("desktop.rs").is_some());
        back.emit_clicked();
        wait(&|| !back.is_visible() && position("Cargo.toml").is_some());

        // Exercise chooser selection and cancellation without showing a window or launching an app.
        let explorer = &view.explorer;
        explorer.selection.unselect_all();
        assert!(explorer.open_with_dialog().is_none());
        explorer
            .selection
            .select_item(position("Cargo.toml").unwrap(), true);
        let dialog = explorer.open_with_dialog().unwrap();
        assert!(
            dialog
                .gfile()
                .unwrap()
                .equal(&explorer.current.borrow().child("Cargo.toml"))
        );
        let previous_error = explorer.error_label.text();
        dialog.response(gtk::ResponseType::Cancel);
        assert_eq!(explorer.error_label.text(), previous_error);
        explorer
            .selection
            .select_item(position("src").unwrap(), false);
        assert!(explorer.open_with_dialog().is_none());

        let more = view.input_widgets[5]
            .clone()
            .downcast::<gtk::MenuButton>()
            .unwrap();
        let more_menu = more.popover().unwrap().child().unwrap();
        assert_eq!(
            more_menu
                .first_child()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .label()
                .as_deref(),
            Some("Operation results")
        );
        assert_eq!(
            more_menu
                .last_child()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .label()
                .as_deref(),
            Some("Quit Kora")
        );

        let item: gtk::ListItem = glib::Object::new();
        let weak_item = item.downgrade();
        view.grid
            .factory()
            .unwrap()
            .emit_by_name::<()>("setup", &[&item]);
        drop(item);
        assert!(
            weak_item.upgrade().is_none(),
            "drop handlers must not retain retired list items"
        );

        let current = explorer.current.borrow().clone();
        let back_before = explorer.back.borrow().clone();
        let forward_before = explorer.forward.borrow().clone();
        explorer.go_forward();
        explorer.go_forward();
        wait(&|| explorer.current.borrow().path().unwrap().ends_with("src"));
        assert_eq!(explorer.back.borrow().len(), back_before.len() + 1);
        assert_eq!(explorer.forward.borrow().len(), forward_before.len() - 1);
        explorer.go_back();
        wait(&|| explorer.current.borrow().equal(&current));
        let missing = gio::File::for_path(temp_path("missing-history"));
        explorer.back.borrow_mut().push(missing);
        explorer.go_back();
        wait(&|| explorer.recovery.is_visible());
        assert!(explorer.current.borrow().equal(&current));
        assert_eq!(explorer.back.borrow().len(), back_before.len() + 1);
        assert_eq!(*explorer.forward.borrow(), forward_before);
        explorer.back.borrow_mut().pop();

        let directory = temp_path("refresh");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("before"), "before").unwrap();
        explorer.navigate(gio::File::for_path(&directory), Navigation::Visit);
        wait(&|| !explorer.directory.is_loading() && position("before").is_some());
        explorer.directory.set_monitored(false);
        let back_before = explorer.back.borrow().clone();
        let forward_before = explorer.forward.borrow().clone();
        fs::write(directory.join("after"), "after").unwrap();
        fs::write(directory.join(".hidden"), "hidden").unwrap();
        assert!(position("after").is_none());
        explorer.refresh();
        wait(&|| !explorer.directory.is_loading() && position("after").is_some());
        assert!(position(".hidden").is_none());
        assert_eq!(explorer.current.borrow().path().unwrap(), directory);
        assert_eq!(*explorer.back.borrow(), back_before);
        assert_eq!(*explorer.forward.borrow(), forward_before);
        let moved = temp_path("refresh-moved");
        fs::rename(&directory, &moved).unwrap();
        explorer.refresh();
        wait(&|| explorer.recovery.is_visible());
        assert!(
            position("after").is_some(),
            "failed refresh must preserve the visible listing"
        );
        fs::rename(&moved, &directory).unwrap();

        let retry_target = temp_path("retry");
        explorer.navigate(gio::File::for_path(&retry_target), Navigation::Visit);
        wait(&|| {
            explorer.path_entry.text().as_str() == retry_target.to_string_lossy().as_ref()
                && explorer.recovery.is_visible()
        });
        // Let the pending request fail before creating the folder it will retry.
        wait(&|| {
            explorer
                .error_label
                .text()
                .contains(retry_target.to_string_lossy().as_ref())
        });
        fs::create_dir(&retry_target).unwrap();
        fs::write(retry_target.join("retried"), "retried").unwrap();
        explorer
            .recovery
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        wait(&|| position("retried").is_some());
        assert_eq!(explorer.current.borrow().path().unwrap(), retry_target);
        explorer.directory.set_file(gio::File::NONE);
        fs::remove_dir_all(retry_target).unwrap();
        fs::remove_dir_all(directory).unwrap();

        let queue = OperationQueue::new(&app);
        let completed = Rc::new(Cell::new(false));
        queue
            .enqueue_with_callback(Operation::Trash { paths: vec![] }, {
                let queue = queue.clone();
                let completed = completed.clone();
                move |success| {
                    assert!(!success);
                    queue
                        .enqueue_with_callback(Operation::Trash { paths: vec![] }, move |success| {
                            assert!(!success);
                            completed.set(true);
                        })
                        .unwrap();
                }
            })
            .unwrap();
        wait(&|| completed.get());
        assert!(!queue.is_active());
        drop(queue);
        std::thread::sleep(std::time::Duration::from_millis(20));
        wait(&|| !glib::MainContext::default().pending());

        let weak_more = more.popover().unwrap().downgrade();
        let weak_grid = view.grid.downgrade();
        let weak_explorer = Rc::downgrade(explorer);
        drop(more_menu);
        drop(more);
        drop(back);
        drop(hidden);
        drop(view);
        assert!(
            weak_explorer.upgrade().is_none(),
            "views must release explorer state"
        );
        assert!(
            weak_grid.upgrade().is_none(),
            "views must release their grid"
        );
        assert!(
            weak_more.upgrade().is_none(),
            "menu callbacks must not retain their parent popover"
        );
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
