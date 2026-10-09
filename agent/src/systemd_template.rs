use askama::Template;

#[derive(Template)]
#[template(path = "app.service", escape = "none")]
pub struct AppServiceTemplate<'a> {
    pub name: &'a str,
    pub working_dir: &'a str,
    pub exec_start: &'a str,
    pub user: &'a str,
    pub env_file: &'a str,
    pub port: u16,
}
