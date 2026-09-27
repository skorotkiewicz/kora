use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use gtk::{gio, glib, prelude::*};
use gtk4 as gtk;
use rustix::fs::{CWD, OFlags, RenameFlags, renameat_with};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalDragItem {
    path: PathBuf,
    fingerprint: Fingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    Copy {
        sources: Vec<PathBuf>,
        destination: PathBuf,
    },
    Move {
        sources: Vec<PathBuf>,
        destination: PathBuf,
    },
    Rename {
        source: PathBuf,
        new_name: OsString,
    },
    Trash {
        paths: Vec<PathBuf>,
    },
    FinishExternalMove {
        items: Vec<ExternalDragItem>,
    },
}

#[derive(Debug, Clone)]
pub struct Request {
    pub id: u64,
    pub operation: Operation,
}

#[derive(Debug, Clone)]
pub struct ItemResult {
    pub path: PathBuf,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OperationResult {
    pub id: u64,
    pub items: Vec<ItemResult>,
}

impl OperationResult {
    pub fn message(&self) -> String {
        let failed = self
            .items
            .iter()
            .filter(|item| item.error.is_some())
            .count();
        if failed == 0 {
            format!("Operation {} completed", self.id)
        } else {
            let details = self
                .items
                .iter()
                .filter_map(|item| {
                    item.error
                        .as_ref()
                        .map(|error| format!("{}: {error}", item.path.display()))
                })
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "Operation {} completed with {failed} of {} items failed: {details}",
                self.id,
                self.items.len()
            )
        }
    }
}

type CompletionCallbacks = Rc<std::cell::RefCell<HashMap<u64, Box<dyn FnOnce(bool)>>>>;
type IdleCallbacks = Rc<std::cell::RefCell<Vec<Box<dyn FnOnce()>>>>;

pub struct OperationQueue {
    sender: mpsc::Sender<Request>,
    listeners: Rc<std::cell::RefCell<Vec<glib::WeakRef<gtk::Label>>>>,
    results: Arc<Mutex<VecDeque<OperationResult>>>,
    completion_callbacks: CompletionCallbacks,
    holds: Rc<std::cell::RefCell<VecDeque<gio::ApplicationHoldGuard>>>,
    clipboard: std::cell::RefCell<Option<(bool, Vec<PathBuf>)>>,
    idle_callbacks: IdleCallbacks,
    internal_drag_moves: std::cell::RefCell<Vec<Vec<PathBuf>>>,
    next_id: std::cell::Cell<u64>,
    app: gtk::Application,
}

impl OperationQueue {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let (sender, receiver) = mpsc::channel::<Request>();
        let (mut reader, mut writer) = UnixStream::pair().expect("create operation event pipe");
        reader
            .set_nonblocking(true)
            .expect("make operation event pipe nonblocking");
        let results = Arc::new(Mutex::new(VecDeque::new()));
        let completed = Arc::new(Mutex::new(VecDeque::new()));
        let worker_results = results.clone();
        let worker_completed = completed.clone();
        thread::Builder::new()
            .name("kora-files".into())
            .spawn(move || {
                // ponytail: one worker serializes mutations; add per-device workers only if measured throughput requires it.
                let mut seen = HashSet::new();
                while let Ok(request) = receiver.recv() {
                    let result = execute_once(&mut seen, request);
                    if seen.len() > 1024 {
                        seen.clear();
                        seen.insert(result.id);
                    }
                    worker_completed.lock().unwrap().push_back(result.clone());
                    let mut stored = worker_results.lock().unwrap();
                    stored.push_back(result);
                    while stored.len() > 100 {
                        stored.pop_front();
                    }
                    drop(stored);
                    let _ = writer.write_all(&[1]);
                }
            })
            .expect("start filesystem worker");

        let listeners = Rc::new(std::cell::RefCell::new(
            Vec::<glib::WeakRef<gtk::Label>>::new(),
        ));
        let holds = Rc::new(std::cell::RefCell::new(
            VecDeque::<gio::ApplicationHoldGuard>::new(),
        ));
        let idle_callbacks = Rc::new(std::cell::RefCell::new(Vec::<Box<dyn FnOnce()>>::new()));
        let completion_callbacks = Rc::new(std::cell::RefCell::new(HashMap::<
            u64,
            Box<dyn FnOnce(bool)>,
        >::new()));
        let source_completed = completed.clone();
        let source_callbacks = completion_callbacks.clone();
        let source_listeners = listeners.clone();
        let source_holds = holds.clone();
        let source_idle_callbacks = idle_callbacks.clone();
        glib::source::unix_fd_add_local(reader.as_raw_fd(), glib::IOCondition::IN, move |_, _| {
            let mut bytes = [0; 64];
            match reader.read(&mut bytes) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return glib::ControlFlow::Break,
            }
            let completed = source_completed
                .lock()
                .unwrap()
                .drain(..)
                .collect::<Vec<_>>();
            let message = completed.last().map(OperationResult::message);
            source_listeners.borrow_mut().retain(|weak| {
                if let Some(label) = weak.upgrade() {
                    if let Some(message) = &message {
                        label.set_label(message);
                        label.set_visible(true);
                    }
                    true
                } else {
                    false
                }
            });
            for result in completed {
                source_holds.borrow_mut().pop_front();
                if let Some(callback) = source_callbacks.borrow_mut().remove(&result.id) {
                    callback(result.items.iter().all(|item| item.error.is_none()));
                }
            }
            if source_holds.borrow().is_empty() {
                for callback in source_idle_callbacks.take() {
                    callback();
                }
            }
            glib::ControlFlow::Continue
        });

        Rc::new(Self {
            sender,
            listeners,
            results,
            completion_callbacks,
            holds,
            clipboard: std::cell::RefCell::new(None),
            idle_callbacks,
            internal_drag_moves: std::cell::RefCell::new(Vec::new()),
            next_id: std::cell::Cell::new(1),
            app: app.clone(),
        })
    }

    pub fn subscribe(&self, label: &gtk::Label) {
        let weak = glib::WeakRef::new();
        weak.set(Some(label));
        self.listeners.borrow_mut().push(weak);
        if let Some(result) = self.results.lock().unwrap().back() {
            label.set_label(&result.message());
            label.set_visible(true);
        }
    }

    pub fn enqueue(&self, operation: Operation) -> Result<u64, String> {
        self.enqueue_with_callback(operation, |_| {})
    }

    pub fn enqueue_with_callback(
        &self,
        operation: Operation,
        callback: impl FnOnce(bool) + 'static,
    ) -> Result<u64, String> {
        let id = self.next_id.get();
        self.next_id.set(id.wrapping_add(1));
        self.completion_callbacks
            .borrow_mut()
            .insert(id, Box::new(callback));
        if let Err(error) = self.submit(Request { id, operation }) {
            self.completion_callbacks.borrow_mut().remove(&id);
            return Err(error);
        }
        Ok(id)
    }

    pub fn snapshot_external_drag(
        &self,
        paths: &[PathBuf],
    ) -> Result<Vec<ExternalDragItem>, String> {
        validate_sources(paths.to_vec(), None)?
            .iter()
            .map(|path| {
                Ok(ExternalDragItem {
                    path: path.clone(),
                    fingerprint: fingerprint(path)?,
                })
            })
            .collect()
    }

    pub fn mark_internal_drag_move(&self, mut paths: Vec<PathBuf>) {
        paths.sort();
        self.internal_drag_moves.borrow_mut().push(paths);
    }

    pub fn consume_internal_drag_move(&self, paths: &[PathBuf]) -> bool {
        let mut paths = paths.to_vec();
        paths.sort();
        let mut moves = self.internal_drag_moves.borrow_mut();
        let Some(index) = moves.iter().position(|candidate| *candidate == paths) else {
            return false;
        };
        moves.remove(index);
        true
    }

    pub fn set_clipboard(&self, paths: Vec<PathBuf>, move_files: bool) {
        *self.clipboard.borrow_mut() = Some((move_files, paths));
    }

    pub fn clipboard(&self) -> Option<(bool, Vec<PathBuf>)> {
        self.clipboard.borrow().clone()
    }

    pub fn is_active(&self) -> bool {
        !self.holds.borrow().is_empty()
    }

    pub fn quit_when_idle(&self) {
        let app = self.app.clone();
        self.when_idle(move || app.quit());
    }

    pub fn result_messages(&self) -> String {
        self.results
            .lock()
            .unwrap()
            .iter()
            .map(OperationResult::message)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn when_idle(&self, callback: impl FnOnce() + 'static) {
        if self.is_active() {
            self.idle_callbacks.borrow_mut().push(Box::new(callback));
        } else {
            callback();
        }
    }

    fn submit(&self, request: Request) -> Result<(), String> {
        self.holds.borrow_mut().push_back(self.app.hold());
        if self.sender.send(request).is_err() {
            self.holds.borrow_mut().pop_back();
            return Err("filesystem worker stopped".into());
        }
        for weak in self.listeners.borrow().iter() {
            if let Some(label) = weak.upgrade() {
                label.set_label("File operation in progress");
                label.set_visible(true);
            }
        }
        Ok(())
    }
}

fn execute_once(seen: &mut HashSet<u64>, request: Request) -> OperationResult {
    if seen.insert(request.id) {
        execute(request)
    } else {
        OperationResult {
            id: request.id,
            items: vec![ItemResult {
                path: PathBuf::new(),
                error: Some("duplicate operation request ignored".into()),
            }],
        }
    }
}

fn execute(request: Request) -> OperationResult {
    let id = request.id;
    let items = match validate(request.operation) {
        Ok(Operation::Copy {
            sources,
            destination,
        }) => sources
            .into_iter()
            .map(|source| {
                let target = destination.join(source.file_name().unwrap_or_default());
                item_result(
                    source.clone(),
                    copy_entry(&source, &target).map_err(|error| {
                        format!("{error}; destination may be partial: {}", target.display())
                    }),
                )
            })
            .collect(),
        Ok(Operation::Move {
            sources,
            destination,
        }) => sources
            .into_iter()
            .map(|source| {
                let target = destination.join(source.file_name().unwrap_or_default());
                item_result(
                    source.clone(),
                    move_entry(&source, &target).map_err(|error| {
                        format!(
                            "{error}; check source and destination: {}",
                            target.display()
                        )
                    }),
                )
            })
            .collect(),
        Ok(Operation::Rename { source, new_name }) => {
            let target = source.with_file_name(new_name);
            vec![item_result(
                source.clone(),
                rename_no_replace(&source, &target),
            )]
        }
        Ok(Operation::Trash { paths }) => paths
            .into_iter()
            .map(|path| item_result(path.clone(), trash_path(&path)))
            .collect(),
        Ok(Operation::FinishExternalMove { items }) => items
            .into_iter()
            .map(|item| {
                let result = remove_verified(&item.path, &item.fingerprint);
                item_result(item.path, result)
            })
            .collect(),
        Err(error) => vec![ItemResult {
            path: PathBuf::new(),
            error: Some(error),
        }],
    };
    OperationResult { id, items }
}

fn trash_path(path: &Path) -> Result<(), String> {
    trash_with(path, |path| {
        gio::File::for_path(path)
            .trash(gio::Cancellable::NONE)
            .map_err(|error| error.to_string())
    })
}

fn trash_with(path: &Path, trash: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
    trash(path)
}

fn item_result(path: PathBuf, result: Result<(), String>) -> ItemResult {
    ItemResult {
        path,
        error: result.err(),
    }
}

fn validate(operation: Operation) -> Result<Operation, String> {
    match operation {
        Operation::Copy {
            sources,
            destination,
        } => Ok(Operation::Copy {
            sources: validate_sources(sources, Some(&destination))?,
            destination: validate_destination(destination)?,
        }),
        Operation::Move {
            sources,
            destination,
        } => Ok(Operation::Move {
            sources: validate_sources(sources, Some(&destination))?,
            destination: validate_destination(destination)?,
        }),
        Operation::Rename { source, new_name } => {
            validate_source(&source)?;
            let name_path = Path::new(&new_name);
            if new_name.is_empty()
                || name_path.file_name() != Some(new_name.as_os_str())
                || name_path.components().count() != 1
            {
                return Err("invalid destination name".into());
            }
            Ok(Operation::Rename { source, new_name })
        }
        Operation::Trash { paths } => Ok(Operation::Trash {
            paths: validate_sources(paths, None)?,
        }),
        Operation::FinishExternalMove { items } => {
            if items.is_empty() {
                return Err("external move contains no source items".into());
            }
            for item in &items {
                validate_source(&item.path)?;
            }
            Ok(Operation::FinishExternalMove { items })
        }
    }
}

fn validate_destination(destination: PathBuf) -> Result<PathBuf, String> {
    let metadata = fs::metadata(&destination).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", destination.display()));
    }
    Ok(destination)
}

fn validate_sources(
    mut sources: Vec<PathBuf>,
    destination: Option<&Path>,
) -> Result<Vec<PathBuf>, String> {
    if sources.is_empty() {
        return Err("no source items selected".into());
    }
    sources.sort_by_key(|path| path.components().count());
    sources.dedup();
    let mut normalized = Vec::<PathBuf>::new();
    for source in sources {
        validate_source(&source)?;
        if normalized.iter().any(|parent| source.starts_with(parent)) {
            continue;
        }
        if let Some(destination) = destination {
            let destination_resolved =
                fs::canonicalize(destination).map_err(|error| error.to_string())?;
            // Only real directories have descendants. Canonicalizing a link follows
            // its target and incorrectly rejects dangling links and links to ancestors.
            if fs::symlink_metadata(&source)
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                let source_resolved =
                    fs::canonicalize(&source).map_err(|error| error.to_string())?;
                if destination_resolved.starts_with(&source_resolved) {
                    return Err(format!(
                        "cannot transfer {} into itself or its descendant",
                        source.display()
                    ));
                }
            }
        }
        normalized.push(source);
    }
    Ok(normalized)
}

fn validate_source(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    let kind = metadata.file_type();
    if kind.is_file() || kind.is_dir() || kind.is_symlink() {
        Ok(())
    } else {
        Err(format!("unsupported special file: {}", path.display()))
    }
}

fn copy_entry(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|error| error.to_string())?;
    let kind = metadata.file_type();
    if kind.is_symlink() {
        let target = fs::read_link(source).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(target, destination).map_err(|error| error.to_string())
    } else if kind.is_file() {
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
            .open(source)
            .map_err(|error| error.to_string())?;
        let opened = input.metadata().map_err(|error| error.to_string())?;
        if !opened.is_file() || opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err("source changed before copying; source retained".into());
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(destination)
            .map_err(|error| error.to_string())?;
        std::io::copy(&mut input, &mut output).map_err(|error| error.to_string())?;
        output
            .set_permissions(metadata.permissions())
            .map_err(|error| error.to_string())?;
        output.sync_all().map_err(|error| error.to_string())
    } else if kind.is_dir() {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(destination)
            .map_err(|error| error.to_string())?;
        for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, metadata.permissions()).map_err(|error| error.to_string())
    } else {
        Err(format!("unsupported special file: {}", source.display()))
    }
}

fn rename_no_replace(source: &Path, destination: &Path) -> Result<(), String> {
    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE)
        .map_err(|error| error.to_string())
}

fn move_entry(source: &Path, destination: &Path) -> Result<(), String> {
    match renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::XDEV) => cross_filesystem_move(source, destination, || {}),
        Err(error) => Err(error.to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    mode: u32,
    children: BTreeMap<OsString, Fingerprint>,
}

fn fingerprint(path: &Path) -> Result<Fingerprint, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    validate_source(path)?;
    let mut children = BTreeMap::new();
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            children.insert(entry.file_name(), fingerprint(&entry.path())?);
        }
    }
    Ok(Fingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        mode: metadata.mode(),
        children,
    })
}

fn remove_verified(path: &Path, expected: &Fingerprint) -> Result<(), String> {
    if fingerprint(path)? != *expected {
        return Err(format!(
            "source changed during transfer; retained {}",
            path.display()
        ));
    }
    remove_verified_entries(path, expected)
}

fn remove_verified_entries(path: &Path, expected: &Fingerprint) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.dev() != expected.device
        || metadata.ino() != expected.inode
        || metadata.mode() != expected.mode
    {
        return Err(format!(
            "source changed during cleanup; retained {}",
            path.display()
        ));
    }
    if metadata.is_dir() {
        for (name, child) in &expected.children {
            remove_verified_entries(&path.join(name), child)?;
        }
        // Never remove_dir_all: a new, uncopied entry must prevent directory removal.
        fs::remove_dir(path).map_err(|error| error.to_string())
    } else if fingerprint(path)? == *expected {
        fs::remove_file(path).map_err(|error| error.to_string())
    } else {
        Err(format!(
            "source changed during cleanup; retained {}",
            path.display()
        ))
    }
}

fn cross_filesystem_move(
    source: &Path,
    destination: &Path,
    after_copy: impl FnOnce(),
) -> Result<(), String> {
    let before = fingerprint(source)?;
    copy_entry(source, destination)?;
    after_copy();
    if fingerprint(source)? != before {
        return Err(format!(
            "source changed during move; retained source and destination at {}",
            destination.display()
        ));
    }
    remove_verified(source, &before)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kora-operations-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn external_drag_deletes_only_after_successful_move_completion() {
        let root = temp_dir("external-drag");
        let copied = root.join("copied");
        let cancelled = root.join("cancelled");
        let changed = root.join("changed");
        fs::write(&copied, "copied").unwrap();
        fs::write(&cancelled, "cancelled").unwrap();
        fs::write(&changed, "before").unwrap();
        let copied_item = ExternalDragItem {
            path: copied.clone(),
            fingerprint: fingerprint(&copied).unwrap(),
        };
        let changed_item = ExternalDragItem {
            path: changed.clone(),
            fingerprint: fingerprint(&changed).unwrap(),
        };

        assert!(copied.exists());
        assert!(cancelled.exists());
        fs::write(&changed, "changed after drag").unwrap();
        let result = execute(Request {
            id: 9,
            operation: Operation::FinishExternalMove {
                items: vec![copied_item, changed_item],
            },
        });

        assert!(!copied.exists());
        assert!(cancelled.exists());
        assert_eq!(fs::read_to_string(&changed).unwrap(), "changed after drag");
        assert_eq!(
            result
                .items
                .iter()
                .filter(|item| item.error.is_some())
                .count(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn external_directory_move_retains_changed_children() {
        let root = temp_dir("external-directory");
        let source = root.join("tree");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("child"), "before").unwrap();
        let item = ExternalDragItem { path: source.clone(), fingerprint: fingerprint(&source).unwrap() };
        fs::write(source.join("child"), "after transfer").unwrap();
        let result = execute(Request { id: 30, operation: Operation::FinishExternalMove { items: vec![item] } });
        assert!(result.items[0].error.is_some());
        assert_eq!(fs::read_to_string(source.join("child")).unwrap(), "after transfer");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleanup_retains_entries_added_after_the_whole_tree_check() {
        let root = temp_dir("cleanup-new-entry");
        fs::write(root.join("copied"), "copied content").unwrap();
        let before = fingerprint(&root).unwrap();
        fs::write(root.join("not-copied"), "new content").unwrap();
        assert!(remove_verified_entries(&root, &before).is_err());
        assert_eq!(fs::read_to_string(root.join("not-copied")).unwrap(), "new content");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_move_preserves_contents_and_links() {
        let root = temp_dir("directory-move");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/file"), "content").unwrap();
        std::os::unix::fs::symlink("missing", source.join("broken")).unwrap();
        cross_filesystem_move(&source, &destination, || {}).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(destination.join("nested/file")).unwrap(), "content");
        assert_eq!(fs::read_link(destination.join("broken")).unwrap(), PathBuf::from("missing"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn trash_failure_never_falls_back_to_deletion() {
        let root = temp_dir("trash-failure");
        let file = root.join("file");
        fs::write(&file, "content").unwrap();
        let result = trash_with(&file, |_| Err("Trash unavailable".into()));
        assert_eq!(result.unwrap_err(), "Trash unavailable");
        assert_eq!(fs::read_to_string(&file).unwrap(), "content");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalizes_nested_sources_and_rejects_descendant_destination() {
        let root = temp_dir("validation");
        let source = root.join("source");
        let child = source.join("child");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&child).unwrap();
        let normalized = validate_sources(vec![child.clone(), source.clone()], None).unwrap();
        assert_eq!(normalized, vec![source.clone()]);
        assert!(validate_sources(vec![source], Some(&child)).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copies_tree_without_following_links_and_never_overwrites() {
        let root = temp_dir("copy");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), "content").unwrap();
        std::os::unix::fs::symlink("file", source.join("link")).unwrap();
        copy_entry(&source, &destination).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("file")).unwrap(),
            "content"
        );
        assert_eq!(
            fs::read_link(destination.join("link")).unwrap(),
            PathBuf::from("file")
        );
        assert!(copy_entry(&source, &destination).is_err());
        assert_eq!(fs::read_to_string(source.join("file")).unwrap(), "content");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn no_replace_rename_preserves_both_entries_on_collision() {
        let root = temp_dir("rename");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::write(&source, "source").unwrap();
        fs::write(&destination, "destination").unwrap();
        assert!(rename_no_replace(&source, &destination).is_err());
        assert_eq!(fs::read_to_string(source).unwrap(), "source");
        assert_eq!(fs::read_to_string(destination).unwrap(), "destination");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_request_id_is_not_executed_twice() {
        let root = temp_dir("duplicate");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, "content").unwrap();
        let request = Request {
            id: 7,
            operation: Operation::Copy {
                sources: vec![source],
                destination: destination.clone(),
            },
        };
        let mut seen = HashSet::new();
        assert!(
            execute_once(&mut seen, request.clone()).items[0]
                .error
                .is_none()
        );
        assert_eq!(
            execute_once(&mut seen, request).items[0].error.as_deref(),
            Some("duplicate operation request ignored")
        );
        assert_eq!(
            fs::read_to_string(destination.join("source")).unwrap(),
            "content"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_special_files_and_symlinked_descendant_targets() {
        use std::os::unix::net::UnixListener;

        let root = temp_dir("unsafe");
        let socket = root.join("socket");
        let _listener = UnixListener::bind(&socket).unwrap();
        assert!(validate_source(&socket).is_err());

        let source = root.join("source");
        let child = source.join("child");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&child).unwrap();
        let linked_destination = root.join("linked-destination");
        std::os::unix::fs::symlink(&child, &linked_destination).unwrap();
        assert!(validate_sources(vec![source], Some(&linked_destination)).is_err());
        drop(_listener);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_source_is_retained_after_cross_filesystem_copy() {
        let root = temp_dir("changed");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::write(&source, "before").unwrap();
        let result = cross_filesystem_move(&source, &destination, || {
            fs::write(&source, "changed after copy").unwrap();
        });
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(source).unwrap(), "changed after copy");
        assert_eq!(fs::read_to_string(destination).unwrap(), "before");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_child_is_retained_after_directory_copy() {
        let root = temp_dir("changed-child");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("child"), "before").unwrap();
        let result = cross_filesystem_move(&source, &destination, || {
            fs::write(source.join("child"), "after copying").unwrap();
        });
        assert!(
            result.is_err(),
            "a changed child must prevent source removal"
        );
        assert_eq!(
            fs::read_to_string(source.join("child")).unwrap(),
            "after copying"
        );
        assert_eq!(
            fs::read_to_string(destination.join("child")).unwrap(),
            "before"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copy_preserves_private_and_executable_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_dir("copy-modes");
        for mode in [0o600, 0o700, 0o640] {
            let source = root.join(format!("source-{mode:o}"));
            let destination = root.join(format!("copy-{mode:o}"));
            fs::write(&source, "content").unwrap();
            fs::set_permissions(&source, fs::Permissions::from_mode(mode)).unwrap();
            copy_entry(&source, &destination).unwrap();
            assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, mode);
        }
        let source = root.join("private-directory");
        fs::create_dir(&source).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        copy_entry(&source, &root.join("directory-copy")).unwrap();
        assert_eq!(
            fs::metadata(root.join("directory-copy")).unwrap().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn broken_symlink_is_transferable_without_following_its_target() {
        let root = temp_dir("broken-link");
        let source = root.join("broken");
        let destination = root.join("destination");
        fs::create_dir(&destination).unwrap();
        std::os::unix::fs::symlink("missing", &source).unwrap();
        let result = execute(Request {
            id: 20,
            operation: Operation::Copy {
                sources: vec![source.clone()],
                destination: destination.clone(),
            },
        });
        assert!(
            result.items[0].error.is_none(),
            "{:?}",
            result.items[0].error
        );
        assert_eq!(
            fs::read_link(destination.join("broken")).unwrap(),
            PathBuf::from("missing")
        );
        let result = execute(Request {
            id: 21,
            operation: Operation::Rename {
                source,
                new_name: "renamed".into(),
            },
        });
        assert!(result.items[0].error.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cross_filesystem_move_removes_source_after_copy() {
        let root = temp_dir("cross-device");
        let shared_memory = PathBuf::from("/dev/shm");
        if !shared_memory.is_dir()
            || fs::metadata(&root).unwrap().dev() == fs::metadata(&shared_memory).unwrap().dev()
        {
            fs::remove_dir(root).unwrap();
            return;
        }
        let source = root.join("source");
        let destination = shared_memory.join(format!(
            "kora-cross-device-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&source, "content").unwrap();
        move_entry(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "content");
        fs::remove_file(destination).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copy_and_cleanup_permission_failures_keep_source_data() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("permissions");
        let source = root.join("source");
        let blocked = root.join("blocked");
        fs::write(&source, "content").unwrap();
        fs::create_dir(&blocked).unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o500)).unwrap();
        assert!(cross_filesystem_move(&source, &blocked.join("destination"), || {}).is_err());
        assert_eq!(fs::read_to_string(&source).unwrap(), "content");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();

        let destination_root = temp_dir("cleanup-destination");
        let destination = destination_root.join("copied");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
        let result = cross_filesystem_move(&source, &destination, || {});
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&source).unwrap(), "content");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "content");
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn failed_cross_filesystem_copy_keeps_source() {
        let root = temp_dir("failed-copy");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::write(&source, "source").unwrap();
        fs::write(&destination, "existing").unwrap();
        assert!(cross_filesystem_move(&source, &destination, || {}).is_err());
        assert_eq!(fs::read_to_string(source).unwrap(), "source");
        assert_eq!(fs::read_to_string(destination).unwrap(), "existing");
        fs::remove_dir_all(root).unwrap();
    }
}
