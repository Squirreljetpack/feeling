use feeling::{
    clap::parse_args, config::Config, db, handlers::handle_command, logger::init_logger, paths::*,
};

use cba::{_dbg, bog};
use cba::{bo::load_type_or_default, bog::BogOkExt};
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() {
    bog::init_bogger(true, false);

    let cli = parse_args().__ebog();

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
