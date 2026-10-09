use crate::error::{AgentError, AgentResult};
use mysql_async::{Pool, PoolOpts};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct PanelDb {
    inner: Arc<Mutex<Connection>>,
}

impl PanelDb {
    pub fn open(path: &Path) -> AgentResult<Self> {
        let conn = Connection::open(path).map_err(AgentError::Sqlite)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(AgentError::Sqlite)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(AgentError::Sqlite)?;
        conn.pragma_update(None, "busy_timeout", 5000_i64)
            .map_err(AgentError::Sqlite)?;
        Ok(PanelDb {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<R>) -> AgentResult<R> {
        let conn = self.inner.lock().expect("panel db lock poisoned");
        f(&conn).map_err(AgentError::Sqlite)
    }
}

#[derive(Clone)]
pub struct MariaDb {
    pool: Pool,
}

impl MariaDb {
    pub fn connect(url: &str) -> AgentResult<Self> {
        let opts = mysql_async::Opts::from_url(url)
            .map_err(|e| AgentError::Other(anyhow::anyhow!("mariadb url: {e}")))?;
        let pool = Pool::new(mysql_async::OptsBuilder::from_opts(opts).pool_opts(
            PoolOpts::default(),
        ));
        Ok(MariaDb { pool })
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }
}
