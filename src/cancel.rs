use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug)]
pub struct Cancel(Option<(Arc<AtomicU64>, u64)>);

impl Cancel {
    pub fn never() -> Self {
        Self(None)
    }

    pub fn new(generation: Arc<AtomicU64>, expected: u64) -> Self {
        Self(Some((generation, expected)))
    }

    pub fn is_cancelled(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|(generation, expected)| generation.load(Ordering::Acquire) != *expected)
    }

    pub fn check(&self) -> std::io::Result<()> {
        if self.is_cancelled() {
            Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "superseded by a newer request",
            ))
        } else {
            Ok(())
        }
    }
}
