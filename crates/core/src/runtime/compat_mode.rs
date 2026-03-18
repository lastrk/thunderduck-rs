use crate::error::{Result, ThunderduckError};
use crate::functions::CompatMode;
use crate::runtime::extension_loader;

/// Requested compat mode (may include Auto for runtime detection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCompatMode {
    /// Always use vanilla DuckDB functions (~85% Spark parity).
    Relaxed,
    /// Always use Spark extension functions (~100% parity). Fails if extension unavailable.
    Strict,
    /// Load extension if available; fall back to Relaxed otherwise.
    Auto,
}

impl RuntimeCompatMode {
    /// Read `THUNDERDUCK_COMPAT_MODE` from the environment.
    /// Defaults to `Auto` when unset or unrecognised.
    pub fn from_env() -> Self {
        let val = std::env::var("THUNDERDUCK_COMPAT_MODE")
            .unwrap_or_default()
            .to_lowercase();
        match val.as_str() {
            "strict" => RuntimeCompatMode::Strict,
            "relaxed" => RuntimeCompatMode::Relaxed,
            _ => RuntimeCompatMode::Auto,
        }
    }
}

impl Default for RuntimeCompatMode {
    fn default() -> Self {
        RuntimeCompatMode::Auto
    }
}

/// Resolve a `RuntimeCompatMode` to a concrete `CompatMode` (Strict or Relaxed),
/// loading the extension into `conn` when required.
pub fn resolve(requested: RuntimeCompatMode, conn: &duckdb::Connection) -> Result<CompatMode> {
    match requested {
        RuntimeCompatMode::Relaxed => Ok(CompatMode::Relaxed),
        RuntimeCompatMode::Strict => {
            let loaded = extension_loader::load(conn)?;
            if loaded {
                Ok(CompatMode::Strict)
            } else {
                Err(ThunderduckError::Unsupported(
                    "strict mode requires the thdck_spark_funcs extension, \
                     but no binary is available for this platform"
                        .into(),
                ))
            }
        }
        RuntimeCompatMode::Auto => {
            let loaded = extension_loader::load(conn)?;
            if loaded {
                Ok(CompatMode::Strict)
            } else {
                Ok(CompatMode::Relaxed)
            }
        }
    }
}
