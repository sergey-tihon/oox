//! Bounded background package work. UI state is never shared with this worker.
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
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
    SavePackage {
        request_id: u64,
        package_path: PathBuf,
        target: PathBuf,
        index: Arc<PackageIndex>,
        edits: Vec<(String, Vec<u8>)>,
    },
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
        matches: Result<Vec<String>, String>,
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
    let file = File::open(path)?;
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
    /// Newest-job-wins queue of pending work. Replaceable jobs are superseded by
    /// later ones, while side-effecting jobs are never dropped.
    queue: Arc<Mutex<VecDeque<Job>>>,
    /// Signals that `queue` is non-empty; it carries no job identity of its own.
    wake: Option<SyncSender<()>>,
    receiver: Receiver<ResultMessage>,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Job {
    /// A transient job only describes a selection the user may have already
    /// moved past, so a newer request makes it obsolete. Exports have side
    /// effects (files, clipboard) and must run exactly once.
    fn is_replaceable(&self) -> bool {
        !matches!(self, Job::ExportPart { .. } | Job::SavePackage { .. })
    }
}

/// Queue policy: a newer transient job supersedes older transient jobs, but
/// side-effecting exports are never displaced, so an export cannot be lost to a
/// selection change or a background search.
fn enqueue(queue: &mut VecDeque<Job>, job: Job) {
    if job.is_replaceable() {
        queue.retain(|queued| !queued.is_replaceable());
    }
    queue.push_back(job);
}

impl Worker {
    pub fn start() -> io::Result<Self> {
        let (results, receiver) = mpsc::sync_channel(2);
        let (wake, wake_receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let worker_stop = Arc::clone(&stop);
        let worker_alive = Arc::clone(&alive);
        let worker_queue = Arc::clone(&queue);
        let thread = thread::Builder::new()
            .name("oox-package-worker".into())
            .spawn(move || {
                let _alive_guard = AliveGuard(worker_alive);
                let mut archive_cache: ArchiveCache = Vec::new();
                'worker: while !worker_stop.load(Ordering::Acquire) {
                    let job = match worker_queue.lock() {
                        Ok(mut queue) => queue.pop_front(),
                        Err(_) => break 'worker,
                    };
                    let Some(job) = job else {
                        // No work: sleep until a submit wakes us. The timeout is a
                        // stop-check backstop, not a polling interval.
                        match wake_receiver.recv_timeout(Duration::from_millis(10)) {
                            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break 'worker,
                        }
                    };
                    let result = match job {
                        Job::Open { request_id, path } => {
                            let (package, summary) = match Package::open(path.clone()) {
                                Ok(mut package) => {
                                    let summary = build_summary(&mut archive_cache, &package);
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
                            let result = compare_packages(&mut archive_cache, &path, &compare_path);
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
                            let matches =
                                search_content(&mut archive_cache, &package_path, &index, &query)
                                    .map_err(|error| error.to_string());
                            ResultMessage::ContentSearch {
                                request_id,
                                query,
                                matches,
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
                            index,
                            edits,
                        } => {
                            let result = save_package(
                                &mut archive_cache,
                                &package_path,
                                &target,
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
            wake: Some(wake),
            receiver,
            stop,
            alive,
            thread: Some(thread),
        })
    }

    /// Queue work without ever waiting on the worker. Only replaceable jobs are
    /// dropped; a newer request supersedes the stale ones instead of growing the
    /// queue.
    pub fn submit(&self, job: Job) -> io::Result<()> {
        let wake = self
            .wake
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "worker is stopped"))?;
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "worker queue is poisoned"))?;
        enqueue(&mut queue, job);
        drop(queue);
        // A full wake channel already has an outstanding signal, and the worker
        // pops one job per iteration, so a dropped signal is not a lost job.
        let _ = wake.try_send(());
        Ok(())
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

fn build_summary(cache: &mut ArchiveCache, package: &Package) -> SummaryPayload {
    let result = cached_archive(cache, &package.source)
        .and_then(|archive| build_document_summary(archive, &package.index));
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
) -> Result<ComparedPackages, String> {
    let mut a = Package::open(path_a).map_err(|error| error.to_string())?;
    let summary = build_summary(cache, &a);
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
    let mut comparison = {
        let (archive_a, archive_b) =
            cached_archives(cache, path_a, path_b).map_err(|error| error.to_string())?;
        compare::compare(&a.index, archive_a, &b.index, archive_b)
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
) -> io::Result<Preview> {
    if package_a == package_b {
        return Ok(Preview::Editor {
            kind: PreviewKind::Diff,
            text: format!("No differences in {part_path}\n"),
            editable: false,
        });
    }
    let (archive_a, archive_b) = cached_archives(cache, package_a, package_b)?;
    compare::diff_part(index_a, archive_a, index_b, archive_b, part_path)
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
) -> io::Result<Vec<String>> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let needle = query.as_bytes();
    let archive = cached_archive(cache, package_path)?;
    let mut matches = Vec::new();

    for part in index.parts.values() {
        if matches.len() >= MAX_CONTENT_SEARCH_RESULTS {
            break;
        }
        if part.kind == PartKind::Directory || part.size > MAX_ENTRY_BYTES {
            continue;
        }
        let Ok(mut entry) = archive.by_name(&part.archive_name) else {
            continue;
        };
        if stream_contains(&mut entry, needle)? {
            matches.push(part.path.clone());
        }
    }
    Ok(matches)
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
/// The new package is built in a sibling temporary file and renamed over the
/// target, so a failure part-way through leaves the original untouched.
fn save_package(
    cache: &mut ArchiveCache,
    package_path: &Path,
    target: &Path,
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
    // The temporary file is owner-only while it is written. The final file takes
    // the permissions of the file it replaces, or of the package it came from
    // when the target is new, so an edit never narrows a shared document.
    let template = std::fs::metadata(target).or_else(|_| std::fs::metadata(package_path));
    if let Ok(template) = template {
        std::fs::set_permissions(temp.path(), template.permissions())?;
    }
    // The cached handles are stale after the rename, and on Windows a read
    // handle would block it. `temp` deletes its file if anything fails here.
    cache.retain(|(cached, _)| cached != target && cached != package_path);
    std::fs::rename(temp.path(), target)?;
    Ok(())
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

fn stream_contains<R: io::Read>(reader: &mut R, needle: &[u8]) -> io::Result<bool> {
    if needle.is_empty() {
        return Ok(true);
    }
    let mut window = Vec::with_capacity(needle.len());
    let mut buffer = [0u8; SEARCH_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(false);
        }
        total = total.saturating_add(count as u64);
        if total > MAX_ENTRY_BYTES {
            return Ok(false);
        }
        for byte in &buffer[..count] {
            window.push(*byte);
            if window.len() > needle.len() {
                window.remove(0);
            }
            if window == needle {
                return Ok(true);
            }
        }
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
    use crate::package::Package;
    use std::{
        collections::VecDeque,
        io::Cursor,
        path::PathBuf,
        sync::Arc,
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
        let found = stream_contains(&mut Cursor::new(bytes), b"needle")
            .expect("stream search should not fail");
        assert!(found);

        let mut input = Cursor::new(b"some text".to_vec());
        assert!(!stream_contains(&mut input, b"missing").unwrap());
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

    #[test]
    fn export_jobs_survive_replaceable_coalescing() {
        let mut queue = VecDeque::new();
        super::enqueue(&mut queue, export_job(7, ExportMode::Clipboard));
        // A burst of replaceable jobs must never displace a queued export.
        for request_id in 0..500 {
            super::enqueue(
                &mut queue,
                Job::Open {
                    request_id,
                    path: PathBuf::from("/missing-package"),
                },
            );
        }

        assert_eq!(queue.len(), 2, "transient jobs coalesce, exports do not");
        assert!(matches!(
            queue.front(),
            Some(Job::ExportPart { request_id: 7, .. })
        ));
        assert!(matches!(queue.back(), Some(Job::Open { .. })));
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
}
