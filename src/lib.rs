pub mod action;
pub mod badge;
pub mod binds;
pub mod clap;
pub mod color;
pub mod color_conversion;
pub mod config;
pub mod date;
pub mod db;
pub mod display;
pub mod editor;
pub mod embed;
pub mod event_loop;
pub mod handlers;
pub mod logger;
pub mod message;
pub mod paths;
pub mod prompts;
pub mod render;
pub mod sql;
pub mod task;
pub mod task_tree;
pub mod tui;
pub mod types;
pub mod utils;
pub mod views;

pub async fn run_app() {
    use cba::{_dbg, bog};
    use cba::{bo::load_type_or_default, bog::BogOkExt};

    bog::init_bogger(true, false);

    let cli = clap::parse_args().__ebog();

    let [q, v] = cli.opts.qv;
    logger::init_logger([q, 1 + v], paths::log_path());

    let mut config: config::Config =
        load_type_or_default(paths::default_config_path(), |s| toml::from_str(s));
    config.init();

    let pool = db::init_database(paths::database_path()).await.__ebog();

    let mut out = std::io::stdout();
    let tui = atty::is(atty::Stream::Stdout);
    _dbg!(handlers::handle_command(cli.cmd, &pool, &config, &cli.opts, &mut out, tui).await)
        .__ebog()
}
