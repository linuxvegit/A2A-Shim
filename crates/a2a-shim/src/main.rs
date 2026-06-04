mod cli;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let parsed = cli::Cli::parse();
    match parsed.command {
        cli::Command::Serve(opts) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
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
        cli::Command::Client(_) => {
            eprintln!("a2a-shim client: implementation lands in Task 33");
            std::process::exit(2);
        }
    }
}
