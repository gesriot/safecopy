#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressPhase {
    Sanity,
    Scanning,
    Copying,
    Cooldown,
    Verifying,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
    Quarantine,
    Retry,
}

#[derive(Debug, Clone)]
pub enum ProgressEvent {
    Phase(ProgressPhase),
    TotalBytes(u64),
    BytesAdvanced(u64),
    CurrentFile(String),
    CooldownLeft(u64),
    Log { level: LogLevel, message: String },
}

pub trait ProgressReporter: Send + Sync {
    fn report(&self, event: ProgressEvent);
}

pub struct NoopReporter;

impl ProgressReporter for NoopReporter {
    fn report(&self, _event: ProgressEvent) {}
}
