use crate::config::Config;
use crate::db::{MariaDb, PanelDb};
use crate::ipc::tasks::TaskTracker;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub panel_db: PanelDb,
    pub mariadb: MariaDb,
    pub tasks: TaskTracker,
}
