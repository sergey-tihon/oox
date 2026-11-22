//! Bounded background package work. UI state is never shared with this worker.
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    cancel::Cancel,
    compare::{self, Comparison},
    package::{self, Diagnostic, MAX_ENTRY_BYTES, Package, PackageIndex, PartInfo, PartKind},
    preview::{Preview, PreviewKind, build_preview},
    summary::{DetailsView, build_document_summary},
};

const MAX_CONTENT_SEARCH_RESULTS: usize = 4096;
const SEARCH_BUFFER_BYTES: usize = 8192;

#[derive(Debug)]
pub enum Job {
    Open {
        request_id: u64,
        path: PathBuf,
    },
    /// Open two packages and compare every part between them.
    Compare {
        request_id: u64,
        path: PathBuf,
        compare_path: PathBuf,
    },
    ReadPart {
        request_id: u64,
        package_path: PathBuf,
        part: Box<PartInfo>,
        index: Arc<PackageIndex>,
    },
    /// Build the part diff between two packages for the content pane.
    DiffPart {
        request_id: u64,
        package_a: PathBuf,
        package_b: PathBuf,
        part_path: String,
        index_a: Arc<PackageIndex>,
        index_b: Arc<PackageIndex>,
    },
    SearchContent {
        request_id: u64,
        package_path: PathBuf,
        query: String,
        index: Arc<PackageIndex>,
    },
    ExportPart {
        request_id: u64,
        package_path: PathBuf,
        part: Box<PartInfo>,
        index: Arc<PackageIndex>,
        mode: ExportMode,
    },
    /// Write a new package with the listed parts replaced, then re-index it for
    /// the UI. `edits` maps package paths to their new contents.
    ///
    /// `overwrite` carries the authorization the user gave on the UI thread: the
    /// UI decides whether a target may be replaced, and the install honors that
    /// decision rather than re-deciding it here. Without it, a file created
    /// between the confirmation and the rename would be clobbered silently.
    SavePackage {
        request_id: u64,
        package_path: PathBuf,
        target: PathBuf,
        overwrite: bool,
        index: Arc<PackageIndex>,
        edits: Vec<(String, Vec<u8>)>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobClass {
    Load,
    Preview,
    Search,
}

impl JobClass {
    fn slot(self) -> usize {
        match self {
            Self::Load => 0,
            Self::Preview => 1,
            Self::Search => 2,
        }
    }
}

/// How a selected part leaves `oox`.
#[derive(Clone, Debug)]
pub enum ExportMode {
    /// Raw, byte-identical part bytes written to this path.
    SaveTo(PathBuf),
    /// Raw, byte-identical part bytes written to a fresh temporary file.
    OpenTemp,
    /// Pretty-printed preview text for the clipboard.
    Clipboard,
}

#[derive(Debug)]
pub enum ExportOutcome {
    Saved(PathBuf),
    TempFile(TempPart),
    Clipboard(String),
}

/// A temporary snapshot that deletes itself on drop. Cleanup then survives
/// every path an export result can take: a superseded request, a worker still
/// holding the result at shutdown, a disconnected result channel, or an error
/// on the way to the UI.
#[derive(Debug)]
pub struct TempPart(PathBuf);

impl TempPart {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPart {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Debug)]
pub struct SummaryPayload {
    pub view: Option<DetailsView>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug)]
pub struct ContentSearchResult {
    pub matches: Vec<String>,
    pub skipped_parts: usize,
}

/// Two open packages plus their comparison, ready for the UI.
#[derive(Debug)]
pub struct ComparedPackages {
    pub a: Package,
    pub b: Package,
    pub summary: SummaryPayload,
    pub comparison: Comparison,
}

#[derive(Debug)]
pub enum ResultMessage {
    Opened {
        request_id: u64,
        path: PathBuf,
        package: Box<Result<Package, String>>,
        summary: Box<SummaryPayload>,
    },
    PartRead {
        request_id: u64,
        selected_path: String,
        preview: Result<Preview, String>,
    },
    Compared {
        request_id: u64,
        path: PathBuf,
        compare_path: PathBuf,
        result: Box<Result<ComparedPackages, String>>,
    },
    ContentSearch {
        request_id: u64,
        query: String,
        result: Result<ContentSearchResult, String>,
    },
    Exported {
        request_id: u64,
        outcome: Result<ExportOutcome, String>,
    },
    Saved {
        request_id: u64,
        path: PathBuf,
        /// The write outcome. Well-formedness warnings are shown before the
        /// write, so a success carries no payload.
        result: Result<(), String>,
    },
}

struct AliveGuard(Arc<AtomicBool>);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Caches the open ZIP archives so per-part preview reads do not re-parse the
/// central directory on every selection change. Two entries are enough for
/// comparison mode, which reads from both packages in turn.
type ArchiveCache = Vec<(PathBuf, zip::ZipArchive<File>)>;

const ARCHIVE_CACHE_CAPACITY: usize = 2;

/// Open `path` if it is not cached, evicting the least recently used archive at
/// capacity. A cache hit is refreshed so the pair used by comparison mode is
/// not evicted between the two reads.
fn ensure_cached(cache: &mut ArchiveCache, path: &Path) -> io::Result<()> {
    if let Some(index) = cache.iter().position(|(cached, _)| cached == path) {
        let entry = cache.remove(index);
        cache.push(entry);
        return Ok(());
    }
    let mut file = File::open(path)?;
    package::check_central_directory(&mut file)?;
    let archive = zip::ZipArchive::new(file)?;
    if cache.len() >= ARCHIVE_CACHE_CAPACITY {
        cache.remove(0);
    }
    cache.push((path.to_path_buf(), archive));
    Ok(())
}

fn cached_archive<'a>(
    cache: &'a mut ArchiveCache,
    path: &Path,
) -> io::Result<&'a mut zip::ZipArchive<File>> {
    ensure_cached(cache, path)?;
    Ok(&mut cache
        .last_mut()
        .expect("the archive cache is non-empty after a successful fill")
        .1)
}

/// Two distinct cached archives borrowed at once. Comparison mode reads the same
/// part from both packages, which needs both handles simultaneously.
fn cached_archives<'a>(
    cache: &'a mut ArchiveCache,
    first: &Path,
    second: &Path,
) -> io::Result<(&'a mut zip::ZipArchive<File>, &'a mut zip::ZipArchive<File>)> {
    ensure_cached(cache, first)?;
    ensure_cached(cache, second)?;
    let i = cache
        .iter()
        .position(|(cached, _)| cached == first)
        .expect("first package was just cached");
    let j = cache
        .iter()
        .position(|(cached, _)| cached == second)
        .expect("second package was just cached");
    if i == j {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "comparison needs two distinct packages",
        ));
    }
    // `split_at_mut` turns the two positions into disjoint mutable borrows.
    if i < j {
        let (left, right) = cache.split_at_mut(j);
        Ok((&mut left[i].1, &mut right[0].1))
    } else {
        let (left, right) = cache.split_at_mut(i);
        Ok((&mut right[0].1, &mut left[j].1))
    }
}

pub struct Worker {
    /// Jobs are coalesced only with pending jobs of the same class.
    queue: Arc<Mutex<VecDeque<QueuedJob>>>,
    generations: [Arc<AtomicU64>; 3],
    /// Signals that `queue` is non-empty; it carries no job identity of its own.
    wake: Option<SyncSender<()>>,
    receiver: Receiver<ResultMessage>,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct QueuedJob {
    job: Job,
    cancel: Cancel,
}

impl Job {
    fn class(&self) -> Option<JobClass> {
        match self {
            Job::Open { .. } | Job::Compare { .. } => Some(JobClass::Load),
            Job::ReadPart { .. } | Job::DiffPart { .. } => Some(JobClass::Preview),
            Job::SearchContent { .. } => Some(JobClass::Search),
            Job::ExportPart { .. } | Job::SavePackage { .. } => None,
        }
    }
}

fn enqueue(queue: &mut VecDeque<QueuedJob>, queued: QueuedJob) {
    if let Some(class) = queued.job.class() {
        queue.retain(|pending| pending.job.class() != Some(class));
    }
    queue.push_back(queued);
}

impl Worker {
    pub fn start() -> io::Result<Self> {
        let (results, receiver) = mpsc::sync_channel(2);
        let (wake, wake_receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let queue = Arc::new(Mutex::new(VecDeque::<QueuedJob>::new()));
        let generations = std::array::from_fn(|_| Arc::new(AtomicU64::new(0)));
        let worker_stop = Arc::clone(&stop);
        let worker_alive = Arc::clone(&alive);
        let worker_queue = Arc::clone(&queue);
        let thread = thread::Builder::new()
            .name("oox-package-worker".into())
            .spawn(move || {
                let _alive_guard = AliveGuard(worker_alive);
                let mut archive_cache: ArchiveCache = Vec::new();
                'worker: while !worker_stop.load(Ordering::Acquire) {
                    let queued = match worker_queue.lock() {
                        Ok(mut queue) => queue.pop_front(),
                        Err(_) => break 'worker,
                    };
                    let Some(QueuedJob { job, cancel }) = queued else {
                        // No work: sleep until a submit wakes us. The timeout is a
                        // stop-check backstop, not a polling interval.
                        match wake_receiver.recv_timeout(Duration::from_millis(10)) {
                            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break 'worker,
                        }
                    };
                    if cancel.is_cancelled() {
                        continue 'worker;
                    }
                    let result = match job {
                        Job::Open { request_id, path } => {
                            let (package, summary) = match Package::open(path.clone()) {
                                Ok(mut package) => {
                                    if cancel.is_cancelled() {
                                        continue 'worker;
                                    }
                                    let summary =
                                        build_summary(&mut archive_cache, &package, &cancel);
                                    // Summary diagnostics belong to the package's
                                    // diagnostic log; merge them before publishing.
                                    for diagnostic in &summary.diagnostics {
                                        Arc::make_mut(&mut package.index)
                                            .record(diagnostic.clone());
                                    }
                                    (Ok(package), summary)
                                }
                                Err(error) => (
                                    Err(error.to_string()),
                                    SummaryPayload {
                                        view: None,
                                        diagnostics: Vec::new(),
                                    },
                                ),
                            };
                            ResultMessage::Opened {
                                request_id,
                                path,
                                package: Box::new(package),
                                summary: Box::new(summary),
                            }
                        }
                        Job::Compare {
                            request_id,
                            path,
                            compare_path,
                        } => {
                            let result =
                                compare_packages(&mut archive_cache, &path, &compare_path, &cancel);
                            ResultMessage::Compared {
                                request_id,
                                path,
                                compare_path,
                                result: Box::new(result),
                            }
                        }
                        Job::DiffPart {
                            request_id,
                            package_a,
                            package_b,
                            part_path,
                            index_a,
                            index_b,
                        } => {
                            let preview = diff_part(
                                &mut archive_cache,
                                &package_a,
                                &package_b,
                                &index_a,
                                &index_b,
                                &part_path,
                                &cancel,
                            )
                            .map_err(|error| error.to_string());
                            ResultMessage::PartRead {
                                request_id,
                                selected_path: part_path,
                                preview,
                            }
                        }
                        Job::ReadPart {
                            request_id,
                            package_path,
                            part,
                            index,
                        } => {
                            let preview =
                                read_preview(&mut archive_cache, &package_path, &part, &index)
                                    .map_err(|error| error.to_string());
                            ResultMessage::PartRead {
                                request_id,
                                selected_path: part.path.clone(),
                                preview,
                            }
                        }
                        Job::SearchContent {
                            request_id,
                            package_path,
                            query,
                            index,
                        } => {
                            let result = search_content(
                                &mut archive_cache,
                                &package_path,
                                &index,
                                &query,
                                &cancel,
                            )
                            .map_err(|error| error.to_string());
                            ResultMessage::ContentSearch {
                                request_id,
                                query,
                                result,
                            }
                        }
                        Job::ExportPart {
                            request_id,
                            package_path,
                            part,
                            index,
                            mode,
                        } => {
                            let outcome =
                                export_part(&mut archive_cache, &package_path, &part, &index, mode)
                                    .map_err(|error| error.to_string());
                            ResultMessage::Exported {
                                request_id,
                                outcome,
                            }
                        }
                        Job::SavePackage {
                            request_id,
                            package_path,
                            target,
                            overwrite,
                            index,
                            edits,
                        } => {
                            let result = save_package(
                                &mut archive_cache,
                                &package_path,
                                &target,
                                overwrite,
                                &index,
                                edits,
                            )
                            .map_err(|error| error.to_string());
                            ResultMessage::Saved {
                                request_id,
                                path: target.clone(),
                                result,
                            }
                        }
                    };
                    if cancel.is_cancelled() {
                        continue 'worker;
                    }
                    // Do not strand the worker when the UI is busy. A bounded,
                    // cooperative retry also lets Drop interrupt shutdown.
                    let mut result = Some(result);
                    while let Some(message) = result.take() {
                        if worker_stop.load(Ordering::Acquire) {
                            break;
                        }
                        match results.try_send(message) {
                            Ok(()) => break,
                            Err(mpsc::TrySendError::Full(message)) => {
                                result = Some(message);
                                thread::sleep(Duration::from_millis(2));
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => break 'worker,
                        }
                    }
                }
            })?;
        Ok(Self {
            queue,
            generations,
            wake: Some(wake),
            receiver,
            stop,
            alive,
            thread: Some(thread),
        })
    }

    /// Queue work without waiting on the worker. Replaceable work is coalesced
    /// only with pending jobs of the same class.
    pub fn submit(&self, job: Job) -> io::Result<()> {
        let wake = self
            .wake
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "worker is stopped"))?;
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "worker queue is poisoned"))?;
        let cancel = match job.class() {
            Some(class) => {
                let generation = Arc::clone(&self.generations[class.slot()]);
                let expected = generation.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
                Cancel::new(generation, expected)
            }
            None => Cancel::never(),
        };
        enqueue(&mut queue, QueuedJob { job, cancel });
        match wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(())) => {
                queue.clear();
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "worker has stopped",
                ))
            }
        }
    }

    pub fn try_recv(&self) -> io::Result<Option<ResultMessage>> {
        match self.receiver.try_recv() {
            Ok(message) => Ok(Some(message)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "worker result channel disconnected",
            )),
        }
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Dropping the wake sender closes the input side. The result receiver is
        // dropped with `self`, so a bounded job can finish without blocking the
        // UI shutdown path. Join only an already-finished thread; otherwise
        // dropping the handle detaches it. Worker jobs own no App state, and the
        // cooperative stop check prevents publishing results after shutdown.
        self.wake.take();
        if let Some(thread) = self.thread.take() {
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                // An in-progress bounded job is intentionally detached rather
                // than freezing the UI. Its owned resources are released when
                // it returns.
                drop(thread);
            }
        }
    }
}

fn build_summary(cache: &mut ArchiveCache, package: &Package, cancel: &Cancel) -> SummaryPayload {
    let result = cached_archive(cache, &package.source)
        .and_then(|archive| build_document_summary(archive, &package.index, cancel));
    match result {
        Ok(view) => SummaryPayload {
            view,
            diagnostics: Vec::new(),
        },
        Err(error) => SummaryPayload {
            view: None,
            diagnostics: vec![Diagnostic::error(
                "summary",
                None,
                format!("summary parser rejected package XML: {error}"),
            )],
        },
    }
}

/// Open both packages, build the primary summary, and compare every part. A
/// comparison of a package with itself is short-circuited: the parts cannot
/// differ, and the archive cache could not hold two handles to one path.
fn compare_packages(
    cache: &mut ArchiveCache,
    path_a: &Path,
    path_b: &Path,
    cancel: &Cancel,
) -> Result<ComparedPackages, String> {
    let mut a = Package::open(path_a).map_err(|error| error.to_string())?;
    cancel.check().map_err(|error| error.to_string())?;
    let summary = build_summary(cache, &a, cancel);
    cancel.check().map_err(|error| error.to_string())?;
    for diagnostic in &summary.diagnostics {
        Arc::make_mut(&mut a.index).record(diagnostic.clone());
    }

    if path_a == path_b {
        return Ok(ComparedPackages {
            comparison: Comparison::identical(&a.index),
            b: a.clone(),
            a,
            summary,
        });
    }

    let b = Package::open(path_b).map_err(|error| error.to_string())?;
    cancel.check().map_err(|error| error.to_string())?;
    let mut comparison = {
        let (archive_a, archive_b) =
            cached_archives(cache, path_a, path_b).map_err(|error| error.to_string())?;
        compare::compare(&a.index, archive_a, &b.index, archive_b, cancel)
    };
    for diagnostic in &comparison.diagnostics {
        Arc::make_mut(&mut a.index).record(diagnostic.clone());
    }
    comparison.diagnostics.clear();

    Ok(ComparedPackages {
        a,
        b,
        summary,
        comparison,
    })
}

/// Read one part out of both packages and render its diff. A package compared
/// with itself has no differences to read for.
fn diff_part(
    cache: &mut ArchiveCache,
    package_a: &Path,
    package_b: &Path,
    index_a: &PackageIndex,
    index_b: &PackageIndex,
    part_path: &str,
    cancel: &Cancel,
) -> io::Result<Preview> {
    if package_a == package_b {
        return Ok(Preview::Editor {
            kind: PreviewKind::Diff,
            text: format!("No differences in {part_path}\n"),
            editable: false,
        });
    }
    let (archive_a, archive_b) = cached_archives(cache, package_a, package_b)?;
    compare::diff_part(index_a, archive_a, index_b, archive_b, part_path, cancel)
}

fn read_preview(
    cache: &mut ArchiveCache,
    package_path: &Path,
    part: &PartInfo,
    index: &PackageIndex,
) -> io::Result<Preview> {
    let archive = cached_archive(cache, package_path)?;
    let bytes = index.read_part(archive, &part.path, MAX_ENTRY_BYTES)?;
    Ok(build_preview(
        &part.archive_name,
        part.content_type.as_deref(),
        part.size,
        part.compressed_size,
        &bytes,
    ))
}

fn search_content(
    cache: &mut ArchiveCache,
    package_path: &Path,
    index: &PackageIndex,
    query: &str,
    cancel: &Cancel,
) -> io::Result<ContentSearchResult> {
    if query.is_empty() {
        return Ok(ContentSearchResult {
            matches: Vec::new(),
            skipped_parts: 0,
        });
    }
    let finder = memchr::memmem::Finder::new(query.as_bytes());
    let archive = cached_archive(cache, package_path)?;
    let mut result = ContentSearchResult {
        matches: Vec::new(),
        skipped_parts: 0,
    };

    for part in index.parts.values() {
        cancel.check()?;
        if result.matches.len() >= MAX_CONTENT_SEARCH_RESULTS {
            break;
        }
        if part.kind == PartKind::Directory || part.size > MAX_ENTRY_BYTES {
            continue;
        }
        let Ok(mut entry) = archive.by_name(&part.archive_name) else {
            result.skipped_parts += 1;
            continue;
        };
        match stream_contains(&mut entry, &finder, cancel) {
            Ok(true) => result.matches.push(part.path.clone()),
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            Err(_) => result.skipped_parts += 1,
        }
    }
    Ok(result)
}

/// Read the raw part bytes under the same bound as previews, so export honors
/// the existing per-part read limit.
fn read_bytes(
    cache: &mut ArchiveCache,
    package_path: &Path,
    part: &PartInfo,
    index: &PackageIndex,
) -> io::Result<Vec<u8>> {
    let archive = cached_archive(cache, package_path)?;
    index.read_part(archive, &part.path, MAX_ENTRY_BYTES)
}

fn export_part(
    cache: &mut ArchiveCache,
    package_path: &Path,
    part: &PartInfo,
    index: &PackageIndex,
    mode: ExportMode,
) -> io::Result<ExportOutcome> {
    match mode {
        ExportMode::Clipboard => match read_preview(cache, package_path, part, index)? {
            Preview::Editor { text, .. } => Ok(ExportOutcome::Clipboard(text)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "part has no text preview to copy",
            )),
        },
        ExportMode::SaveTo(destination) => {
            let bytes = read_bytes(cache, package_path, part, index)?;
            write_new_file(&destination, &bytes, false)?;
            Ok(ExportOutcome::Saved(destination))
        }
        ExportMode::OpenTemp => {
            let bytes = read_bytes(cache, package_path, part, index)?;
            Ok(ExportOutcome::TempFile(write_temp_file(
                &part.archive_name,
                &bytes,
            )?))
        }
    }
}

/// Create a new file and write `bytes`. `create_new` refuses to clobber an
/// existing file and does not follow symlinks, so extracting never silently
/// destroys unrelated data. `owner_only` narrows the initial mode to `0600` on
/// Unix, which matters for temporary copies of a possibly private document.
fn write_new_file(path: &Path, bytes: &[u8], owner_only: bool) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        if owner_only {
            options.mode(0o600);
        }
    }
    #[cfg(not(unix))]
    let _ = owner_only;
    options.open(path)?.write_all(bytes)
}

/// Rewrite the package with the edited parts replaced.
///
/// The new package is built in a sibling temporary file and installed onto the
/// target, so a failure part-way through leaves the original untouched. When
/// `overwrite` is false the install refuses to replace an existing file, so a
/// target that appeared after the user confirmed is never silently clobbered.
fn save_package(
    cache: &mut ArchiveCache,
    package_path: &Path,
    target: &Path,
    overwrite: bool,
    index: &PackageIndex,
    edits: Vec<(String, Vec<u8>)>,
) -> io::Result<()> {
    let replacements: BTreeMap<String, Vec<u8>> = edits.into_iter().collect();

    let directory = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let base = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("package");
    let (temp, file) = create_temp_file(&directory, base)?;

    let mut writer = zip::ZipWriter::new(file);
    {
        let source = cached_archive(cache, package_path)?;
        package::write_edited(source, &mut writer, index, &replacements)?;
    }
    writer.finish()?.sync_all()?;

    // Re-open the result before it replaces anything, so a truncated write is
    // caught while the original is still intact.
    zip::ZipArchive::new(File::open(temp.path())?)?;
    // Preserve permissions only from a regular file being replaced. A symlink
    // target must not borrow its referent's mode; for it (or a new target), use
    // the source package's permissions instead.
    let template_permissions = std::fs::symlink_metadata(target)
        .ok()
        .filter(|metadata| metadata.file_type().is_file())
        .map(|metadata| metadata.permissions())
        .or_else(|| {
            std::fs::metadata(package_path)
                .ok()
                .map(|metadata| metadata.permissions())
        });
    if let Some(permissions) = template_permissions {
        std::fs::set_permissions(temp.path(), permissions)?;
    }
    // The cached handles are stale after the install, and on Windows a read
    // handle would block it. `temp` deletes its file if anything fails here.
    cache.retain(|(cached, _)| cached != target && cached != package_path);
    if overwrite {
        // `rename` replaces the target atomically, which is what the user
        // confirmed on the prompt.
        std::fs::rename(temp.path(), target)?;
    } else {
        install_without_replacing(temp.path(), target)?;
    }
    Ok(())
}

/// Install `source` at `target` without replacing any existing directory entry.
///
/// `hard_link` atomically creates the target link only when that path is free;
/// if a file or symlink appeared while the package was being written, it returns
/// `AlreadyExists` and leaves the existing entry untouched. On success, the
/// source remains owned by `TempPart` and is removed best-effort on drop. The
/// temporary file lives beside the target, so the link cannot cross filesystems.
fn hard_link_install_error(target: &Path, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "cannot install new package at {} without replacing an existing target because this filesystem refused a hard link: {error}; choose a filesystem that supports hard links or select an existing target and confirm overwrite",
            target.display()
        ),
    )
}

fn install_without_replacing(source: &Path, target: &Path) -> io::Result<()> {
    match std::fs::hard_link(source, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} was created while the package was being written",
                target.display()
            ),
        )),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
            ) =>
        {
            Err(hard_link_install_error(target, error))
        }
        Err(error) => Err(error),
    }
}

/// A fresh, empty, owner-only file next to `target`. `create_new` refuses to
/// follow a symlink or reuse an existing file, so a predictable name in a
/// shared directory cannot be turned into a write the user did not intend.
fn create_temp_file(directory: &Path, base: &str) -> io::Result<(TempPart, File)> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    for attempt in 0..100 {
        let candidate = directory.join(format!(".{base}.oox-{}-{attempt}.tmp", std::process::id()));
        match options.open(&candidate) {
            Ok(file) => return Ok((TempPart(candidate), file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a temporary save file",
    ))
}

/// Temporary files keep the part's file name so editors and pagers can pick a
/// syntax mode from the extension.
pub(crate) fn write_temp_file(archive_name: &str, bytes: &[u8]) -> io::Result<TempPart> {
    let base = Path::new(archive_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("part");
    let directory = std::env::temp_dir();
    for attempt in 0..100 {
        let candidate = directory.join(format!("oox-{}-{attempt}-{base}", std::process::id()));
        match write_new_file(&candidate, bytes, true) {
            Ok(()) => return Ok(TempPart(candidate)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("could not allocate a temporary file"))
}

fn stream_contains<R: io::Read>(
    reader: &mut R,
    finder: &memchr::memmem::Finder<'_>,
    cancel: &Cancel,
) -> io::Result<bool> {
    let needle = finder.needle();
    if needle.is_empty() {
        return Ok(true);
    }
    let carry_len = needle.len() - 1;
    let mut carry = Vec::with_capacity(carry_len);
    let mut window = Vec::with_capacity(SEARCH_BUFFER_BYTES + carry_len);
    let mut buffer = [0u8; SEARCH_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        cancel.check()?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(false);
        }
        total = total.saturating_add(count as u64);
        if total > MAX_ENTRY_BYTES {
            return Ok(false);
        }
        window.clear();
        window.extend_from_slice(&carry);
        window.extend_from_slice(&buffer[..count]);
        if finder.find(&window).is_some() {
            return Ok(true);
        }
        carry.clear();
        let carry_start = window.len().saturating_sub(carry_len);
        carry.extend_from_slice(&window[carry_start..]);
    }
}

/// A result is applicable only to the request and selection that produced it.
/// Keeping this predicate separate makes stale-result handling deterministic and testable.
pub fn accepts_result(
    result_request: u64,
    current_request: u64,
    result_path: &str,
    selected_path: &str,
) -> bool {
    result_request == current_request && result_path == selected_path
}

#[cfg(test)]
mod tests {
    use super::{
        ExportMode, ExportOutcome, Job, ResultMessage, Worker, accepts_result, stream_contains,
    };
    use crate::{
        cancel::Cancel,
        package::{Package, PartKind},
    };
    use std::{
        collections::VecDeque,
        io::{self, Cursor},
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    };

    /// The sample part used by the export tests, as a ready-to-queue job.
    fn export_job(request_id: u64, mode: ExportMode) -> Job {
        let package = Package::open("data/sample.pptx").expect("sample package should open");
        let part = package
            .index
            .parts
            .get("/[Content_Types].xml")
            .expect("sample part should be indexed")
            .clone();
        Job::ExportPart {
            request_id,
            package_path: package.source.clone(),
            part: Box::new(part),
            index: Arc::clone(&package.index),
            mode,
        }
    }
    fn queued(job: Job) -> super::QueuedJob {
        super::QueuedJob {
            job,
            cancel: Cancel::never(),
        }
    }

    fn search_job(request_id: u64) -> Job {
        let package = Package::open("data/sample.pptx").expect("sample package should open");
        Job::SearchContent {
            request_id,
            package_path: package.source.clone(),
            query: "needle".to_string(),
            index: Arc::clone(&package.index),
        }
    }

    fn preview_job(request_id: u64) -> Job {
        let package = Package::open("data/sample.pptx").expect("sample package should open");
        let part = package
            .index
            .parts
            .values()
            .find(|part| part.kind != PartKind::Directory)
            .expect("sample package should contain a file part")
            .clone();
        Job::ReadPart {
            request_id,
            package_path: package.source.clone(),
            part: Box::new(part),
            index: Arc::clone(&package.index),
        }
    }

    struct SupersedingReader {
        bytes: Cursor<Vec<u8>>,
        generation: Arc<AtomicU64>,
        supersede_on_read: bool,
    }

    impl io::Read for SupersedingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.supersede_on_read {
                self.generation.fetch_add(1, Ordering::AcqRel);
                self.supersede_on_read = false;
            }
            io::Read::read(&mut self.bytes, buffer)
        }
    }

    /// Pump the worker until the export with `request_id` finishes.
    fn wait_for_export(worker: &Worker, request_id: u64) -> ExportOutcome {
        for _ in 0..2_000 {
            match worker.try_recv() {
                Ok(Some(ResultMessage::Exported {
                    request_id: id,
                    outcome,
                })) if id == request_id => {
                    return outcome.expect("export should succeed");
                }
                Ok(Some(_)) => continue,
                Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                Err(error) => panic!("worker result channel failed: {error}"),
            }
        }
        panic!("export {request_id} never completed");
    }

    #[test]
    fn stale_request_is_discarded() {
        assert!(!accepts_result(1, 2, "/a.xml", "/a.xml"));
        assert!(!accepts_result(2, 2, "/a.xml", "/b.xml"));
        assert!(accepts_result(2, 2, "/a.xml", "/a.xml"));
    }

    #[test]
    fn content_search_matches_exact_bytes_across_buffer_boundaries() {
        let mut bytes = vec![b'x'; super::SEARCH_BUFFER_BYTES - 2];
        bytes.extend_from_slice(b"needle");
        let finder = memchr::memmem::Finder::new(b"needle");
        let found = stream_contains(&mut Cursor::new(bytes), &finder, &Cancel::never())
            .expect("stream search should not fail");
        assert!(found);

        let mut input = Cursor::new(b"some text".to_vec());
        let finder = memchr::memmem::Finder::new(b"missing");
        assert!(!stream_contains(&mut input, &finder, &Cancel::never()).unwrap());
    }

    #[test]
    fn content_search_finds_needle_longer_than_a_chunk() {
        let needle = vec![b'a'; super::SEARCH_BUFFER_BYTES + 100];
        let mut bytes = vec![b'x'; 10];
        bytes.extend_from_slice(&needle);
        let finder = memchr::memmem::Finder::new(&needle);

        assert!(stream_contains(&mut Cursor::new(bytes), &finder, &Cancel::never()).unwrap());
    }

    #[test]
    fn content_search_stops_after_a_newer_request() {
        let generation = Arc::new(AtomicU64::new(4));
        let cancel = Cancel::new(Arc::clone(&generation), 4);
        let mut reader = SupersedingReader {
            bytes: Cursor::new(vec![b'x'; super::SEARCH_BUFFER_BYTES + 1]),
            generation,
            supersede_on_read: true,
        };
        let finder = memchr::memmem::Finder::new(b"missing");

        let error = stream_contains(&mut reader, &finder, &cancel).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn replaceable_jobs_coalesce_independently_by_class() {
        let mut queue = VecDeque::new();
        super::enqueue(&mut queue, queued(export_job(7, ExportMode::Clipboard)));
        super::enqueue(
            &mut queue,
            queued(Job::Open {
                request_id: 1,
                path: PathBuf::from("/old-package"),
            }),
        );
        super::enqueue(&mut queue, queued(search_job(2)));
        super::enqueue(&mut queue, queued(preview_job(3)));
        super::enqueue(
            &mut queue,
            queued(Job::Compare {
                request_id: 4,
                path: PathBuf::from("/new-package"),
                compare_path: PathBuf::from("/other-package"),
            }),
        );
        super::enqueue(&mut queue, queued(search_job(5)));
        super::enqueue(&mut queue, queued(preview_job(6)));

        assert_eq!(
            queue.len(),
            4,
            "each replaceable class keeps only its newest job"
        );
        assert!(matches!(
            queue.front().map(|queued| &queued.job),
            Some(Job::ExportPart { request_id: 7, .. })
        ));
        assert!(matches!(
            queue.get(1).map(|queued| &queued.job),
            Some(Job::Compare { request_id: 4, .. })
        ));
        assert!(matches!(
            queue.get(2).map(|queued| &queued.job),
            Some(Job::SearchContent { request_id: 5, .. })
        ));
        assert!(matches!(
            queue.get(3).map(|queued| &queued.job),
            Some(Job::ReadPart { request_id: 6, .. })
        ));
    }

    #[test]
    fn rapid_submissions_are_nonblocking_and_bounded() {
        let worker = Worker::start().expect("worker should start");
        let started = Instant::now();
        for request_id in 0..10_000 {
            worker
                .submit(Job::Open {
                    request_id,
                    path: PathBuf::from("/missing-package"),
                })
                .expect("worker should retain the newest pending job");
        }
        assert!(started.elapsed().as_millis() < 500, "submission blocked");
    }

    #[cfg(unix)]
    #[test]
    fn temporary_export_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let worker = Worker::start().expect("worker should start");
        worker
            .submit(export_job(11, ExportMode::OpenTemp))
            .expect("export should be queued");

        let ExportOutcome::TempFile(temp) = wait_for_export(&worker, 11) else {
            panic!("expected a temporary file outcome");
        };
        let mode = std::fs::metadata(temp.path())
            .expect("temporary file should exist")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "temporary part copies must stay private");
    }

    #[test]
    fn temp_file_name_keeps_the_part_extension() {
        let temp = super::write_temp_file("ppt/slides/slide1.xml", b"<xml/>")
            .expect("temp file should be created");
        assert_eq!(
            temp.path().extension().and_then(|value| value.to_str()),
            Some("xml")
        );
    }

    #[test]
    fn an_undelivered_temp_result_removes_its_file() {
        let temp =
            super::write_temp_file("slide1.xml", b"<xml/>").expect("temp file should be created");
        let path = temp.path().to_path_buf();
        assert!(path.exists());

        // Dropping the result is what happens when a request is superseded, the
        // result channel is disconnected, or the app quits with it still queued.
        drop(ExportOutcome::TempFile(temp));
        assert!(!path.exists());
    }

    /// The unconfirmed install must refuse to replace a file that appeared after
    /// the user was asked, which a `rename` would silently clobber.
    #[test]
    fn unconfirmed_install_never_replaces_an_existing_file() {
        let directory = std::env::temp_dir().join(format!("oox-install-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create scratch directory");
        let source = directory.join("source.tmp");
        let target = directory.join("target.pptx");

        std::fs::write(&source, b"new").expect("write source");
        std::fs::write(&target, b"existing").expect("write target");
        let error = super::install_without_replacing(&source, &target)
            .expect_err("an existing target must be refused");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&target).expect("target readable"),
            b"existing",
            "the existing file must survive"
        );
        assert!(
            source.exists(),
            "the refused install must leave the source alone"
        );

        // The source remains owned by its temporary-file guard in production.
        std::fs::remove_file(&target).expect("clear target");
        super::install_without_replacing(&source, &target).expect("a free path installs");
        assert_eq!(std::fs::read(&target).expect("target readable"), b"new");
        assert!(source.exists(), "the temporary guard retains its source");
        std::fs::remove_file(&source).expect("clean up source");

        std::fs::remove_file(&target).expect("clean up");
    }

    /// A dangling symlink occupies the path, so an unconfirmed install must not
    /// replace it either.
    #[cfg(unix)]
    #[test]
    fn unconfirmed_install_refuses_a_dangling_symlink() {
        let directory =
            std::env::temp_dir().join(format!("oox-install-link-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create scratch directory");
        let source = directory.join("source.tmp");
        let target = directory.join("target.pptx");
        let missing = directory.join("never-created.pptx");
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&missing);
        std::fs::write(&source, b"new").expect("write source");
        std::os::unix::fs::symlink(&missing, &target).expect("create dangling symlink");

        let error = super::install_without_replacing(&source, &target)
            .expect_err("a dangling symlink must be refused");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(
            std::fs::symlink_metadata(&target)
                .expect("target entry")
                .file_type()
                .is_symlink(),
            "the symlink must survive"
        );

        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&source);
    }

    #[test]
    fn unsupported_hard_link_errors_explain_the_alternatives() {
        let target = PathBuf::from("/shared/output.pptx");
        for kind in [
            std::io::ErrorKind::Unsupported,
            std::io::ErrorKind::PermissionDenied,
        ] {
            let error = super::hard_link_install_error(&target, std::io::Error::from(kind));
            assert_eq!(error.kind(), kind);
            let message = error.to_string();
            assert!(message.contains("hard link"));
            assert!(message.contains("/shared/output.pptx"));
            assert!(message.contains("confirm overwrite"));
        }
    }
}
