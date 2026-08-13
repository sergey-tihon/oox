//! Bounded background package work. UI state is never shared with this worker.
use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    app::{build_document_summary, build_preview, Preview},
    package::{Diagnostic, Package, PackageIndex, MAX_ENTRY_BYTES},
    summary::DetailsView,
};

#[derive(Debug)]
pub enum Job {
    Open {
        request_id: u64,
        path: PathBuf,
    },
    ReadPart {
        request_id: u64,
        package_path: PathBuf,
        selected_path: String,
        archive_name: String,
        content_type: Option<String>,
        size: u64,
        compressed_size: u64,
        index: Box<PackageIndex>,
    },
}

#[derive(Debug)]
pub struct SummaryPayload {
    pub view: Option<DetailsView>,
    pub diagnostics: Vec<Diagnostic>,
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
}

struct AliveGuard(Arc<AtomicBool>);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub struct Worker {
    sender: Option<SyncSender<Job>>,
    receiver: Receiver<ResultMessage>,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    pending: Arc<Mutex<Option<Job>>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn start() -> io::Result<Self> {
        let (sender, jobs) = mpsc::sync_channel(1);
        let (results, receiver) = mpsc::sync_channel(2);
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(Mutex::new(None));
        let worker_stop = Arc::clone(&stop);
        let worker_alive = Arc::clone(&alive);
        let worker_pending = Arc::clone(&pending);
        let thread = thread::Builder::new()
            .name("oox-package-worker".into())
            .spawn(move || {
                let _alive_guard = AliveGuard(worker_alive);
                'worker: while !worker_stop.load(Ordering::Acquire) {
                    // A pending job supersedes anything else waiting in the bounded
                    // queue. This keeps selection changes responsive without growing
                    // an unbounded work queue.
                    let pending_job = worker_pending
                        .lock()
                        .ok()
                        .and_then(|mut pending| pending.take());
                    let job = match pending_job {
                        Some(job) => {
                            // A pending job is newer than anything that filled the
                            // channel. Drain those stale jobs before running it.
                            while jobs.try_recv().is_ok() {}
                            Some(job)
                        }
                        None => match jobs.recv_timeout(Duration::from_millis(10)) {
                            Ok(job) => Some(job),
                            Err(mpsc::RecvTimeoutError::Timeout) => None,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break 'worker,
                        },
                    };
                    let Some(job) = job else { continue };
                    let result = match job {
                        Job::Open { request_id, path } => {
                            let (package, summary) = match Package::open(path.clone()) {
                                Ok(package) => {
                                    let summary = build_summary(&package);
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
                        Job::ReadPart {
                            request_id,
                            package_path,
                            selected_path,
                            archive_name,
                            content_type,
                            size,
                            compressed_size,
                            index,
                        } => {
                            let preview = read_preview(
                                &package_path,
                                &index,
                                &selected_path,
                                &archive_name,
                                content_type.as_deref(),
                                size,
                                compressed_size,
                            )
                            .map_err(|error| error.to_string());
                            ResultMessage::PartRead {
                                request_id,
                                selected_path,
                                preview,
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
            sender: Some(sender),
            receiver,
            stop,
            alive,
            pending,
            thread: Some(thread),
        })
    }

    /// Submit without ever waiting for the worker queue. If it is full, retain
    /// only this newest job; intermediate selections are intentionally skipped.
    pub fn submit(&self, job: Job) -> io::Result<()> {
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "worker is stopped"))?;
        match sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(job)) => {
                let mut pending = self.pending.lock().map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "worker pending queue is stopped")
                })?;
                *pending = Some(job);
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "worker is stopped",
            )),
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
        // Dropping the job sender closes the input side. The result receiver is
        // dropped with `self`, so a bounded job can finish without blocking the
        // UI shutdown path. Join only an already-finished thread; otherwise
        // dropping the handle detaches it. Worker jobs own no App state, and the
        // cooperative stop check prevents publishing results after shutdown.
        self.sender.take();
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

fn build_summary(package: &Package) -> SummaryPayload {
    let result = (|| {
        let file = std::fs::File::open(&package.source)?;
        let mut archive = zip::ZipArchive::new(file)?;
        build_document_summary(&mut archive, &package.index)
    })();
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

fn read_preview(
    package_path: &PathBuf,
    index: &PackageIndex,
    selected_path: &str,
    archive_name: &str,
    content_type: Option<&str>,
    size: u64,
    compressed_size: u64,
) -> io::Result<Preview> {
    let file = std::fs::File::open(package_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let bytes = index.read_part(&mut archive, selected_path, MAX_ENTRY_BYTES)?;
    Ok(build_preview(
        archive_name,
        content_type,
        size,
        compressed_size,
        &bytes,
    ))
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
    use super::{accepts_result, Job, Worker};
    use std::{path::PathBuf, time::Instant};

    #[test]
    fn stale_request_is_discarded() {
        assert!(!accepts_result(1, 2, "/a.xml", "/a.xml"));
        assert!(!accepts_result(2, 2, "/a.xml", "/b.xml"));
        assert!(accepts_result(2, 2, "/a.xml", "/a.xml"));
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
}
