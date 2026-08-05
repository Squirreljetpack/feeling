use feeling::{
    clap::{parse_args, Command}, config::Config, db, handlers::handle_command, logger::init_logger,
    paths::*,
};

use cba::{_dbg, bog};
use cba::{bo::load_type_or_default, bog::BogOkExt};
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() {
    bog::init_bogger(true, false);

    let mut cli = parse_args().__ebog();

    // INCLUDE_COMPLETED / INCLUDE_SCHEDULED env overrides for View commands.
    apply_envs(&mut cli.cmd);

    // -q (quiet) / -v (verbose) map onto init_logger's [q, v] counters.
    // Base is [0, 1] (the historic default): -q drops toward Warn,
    // -v pushes toward Debug.
    let [q, v] = cli.opts.qv;
    init_logger([q, 1 + v], log_path());

    let mut config: Config = load_type_or_default(default_config_path(), |s| toml::from_str(s));
    config.init();

    let pool = db::init_database(database_path()).await.__ebog();

    let mut out = std::io::stdout();
    let tui = atty::is(atty::Stream::Stdout);
    _dbg!(handle_command(cli.cmd, &pool, &config, &cli.opts, &mut out, tui).await).__ebog()
}

/// Apply `INCLUDE_COMPLETED` / `INCLUDE_SCHEDULED` environment overrides to
/// a `Command::View`: each variable set to exactly `true` turns the
/// corresponding flag on (e.g. `INCLUDE_SCHEDULED=true feeling @`). Other
/// values — and the variables on non-View commands — are ignored. There are
/// no CLI flags for these; the env vars are the only way to set them.
fn apply_envs(cmd: &mut Command) {
    if let Command::View {
        include_completed,
        include_scheduled,
        ..
    } = cmd
    {
        if env_flag("INCLUDE_COMPLETED") {
            *include_completed = true;
        }
        if env_flag("INCLUDE_SCHEDULED") {
            *include_scheduled = true;
        }
    }
}

/// `true` when the environment variable is set to exactly `"true"`.
fn env_flag(name: &str) -> bool {
    std::env::var(name).as_deref() == Ok("true")
}
