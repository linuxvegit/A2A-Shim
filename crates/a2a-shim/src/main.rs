mod cli;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let parsed = cli::Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    match parsed.command {
        cli::Command::Serve(opts) => {
            runtime.block_on(async move {
                let r_opts = a2a_shim_serve::run::ServeRuntimeOpts {
                    config_path: opts.config,
                    listen_override: opts.listen,
                    advertised_endpoint_override: opts.advertised_endpoint,
                    cwd_override: opts.cwd,
                    log_file: opts.log_file,
                    log_format: Some(parsed.log_format.clone()),
                };
                if let Err(e) = a2a_shim_serve::run::run(r_opts).await {
                    eprintln!("a2a-shim serve: {e}");
                    std::process::exit(1);
                }
            });
            Ok(())
        }
        cli::Command::Client(opts) => {
            runtime.block_on(async move {
                let r_opts = a2a_shim_client::run::ClientRunOpts {
                    connect_timeout_secs: opts.connect_timeout_secs,
                    stream_idle_secs: opts.stream_idle_secs,
                    hard_ceiling_secs: opts.hard_ceiling_secs,
                    heartbeat_secs: opts.heartbeat_secs,
                    log_file: opts.log_file,
                    log_level: opts.log_level,
                    log_format: Some(parsed.log_format.clone()),
                };
                if let Err(e) = a2a_shim_client::run::run(r_opts).await {
                    // Logs go to stderr; never write to stdout here.
                    eprintln!("a2a-shim client: {e}");
                    std::process::exit(1);
                }
            });
            Ok(())
        }
    }
}
