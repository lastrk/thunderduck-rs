use std::sync::Arc;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use crate::error::Result;
use crate::runtime::compat_mode::RuntimeCompatMode;
use crate::runtime::config::StreamingConfig;
use crate::runtime::session::DuckDbSession;

/// Manages a pool of `DuckDbSession` instances, one per logical Spark session.
///
/// Each session owns a dedicated OS thread with its own in-memory DuckDB database,
/// providing full isolation between sessions.
pub struct SessionManager {
    sessions: DashMap<String, Arc<DuckDbSession>>,
    config: Arc<StreamingConfig>,
    mode: RuntimeCompatMode,
}

impl SessionManager {
    pub fn new(mode: RuntimeCompatMode, config: StreamingConfig) -> Self {
        Self {
            sessions: DashMap::new(),
            config: Arc::new(config),
            mode,
        }
    }

    /// Return the existing session for `session_id`, or create a new one.
    ///
    /// Uses DashMap's `entry` API so the check-and-insert is atomic — concurrent
    /// callers with the same `session_id` will never race to spawn duplicate threads.
    pub async fn get_or_create(&self, session_id: &str) -> Result<Arc<DuckDbSession>> {
        match self.sessions.entry(session_id.to_string()) {
            Entry::Occupied(e) => Ok(Arc::clone(e.get())),
            Entry::Vacant(e) => {
                let session = Arc::new(DuckDbSession::spawn(session_id, self.mode, &self.config)?);
                e.insert(Arc::clone(&session));
                Ok(session)
            }
        }
    }

    /// Release and close the session for `session_id`.
    ///
    /// The `DuckDbSession` is dropped here, which closes the command channel and
    /// causes the session thread to exit.
    pub fn release(&self, session_id: &str) {
        self.sessions.remove(session_id);
    }
}
