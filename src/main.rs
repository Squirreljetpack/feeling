use feeling::{
    clap::parse_args, config::Config, db, handlers::handle_command, logger::init_logger, paths::*,
};

use cba::{_dbg, bog};
use cba::{bo::load_type_or_default, bog::BogOkExt};

#[tokio::main]
async fn main() {
    bog::init_bogger(true, false);

    // Parse the command line (including the leading -q / -v flags) before
    // the logger so the flags can drive its verbosity.
    let cli = parse_args().__ebog();

    // -q (quiet) / -v (verbose) map onto init_logger's [q, v] counters.
    // Base is [0, 1] (the historic default): -q drops toward Warn,
    // -v pushes toward Debug.
    let q = cli.qv.iter().filter(|&&f| f == 'q').count() as u8;
    let v = 1 + cli.qv.iter().filter(|&&f| f == 'v').count() as u8;
    init_logger([q, v], log_path());

    let mut config: Config = load_type_or_default(default_config_path(), |s| toml::from_str(s));
    config.init();

    let pool = db::init_database(database_path()).await.__ebog();

    let mut out = std::io::stdout();
    let tui = atty::is(atty::Stream::Stdout);
    _dbg!(handle_command(cli.cmd, &pool, &config, &mut out, tui).await).__ebog()
}
