use clap::Parser;
use vtamp::cli::{Action, Args, report};

fn main() {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(error) => {
            if error.use_stderr() && std::env::args().any(|a| a == "--json") {
                println!(
                    "{}",
                    serde_json::to_string(&vtamp::model::Reply::failure(
                        vtamp::model::ApiError::new("invalid_arguments", error.to_string())
                    ))
                    .unwrap()
                );
                std::process::exit(2);
            }
            error.exit();
        }
    };
    let json = args.json;
    // The status bar polls frequently. Avoid creating a worker pool per query.
    let mut runtime = if matches!(&args.command, Some(Action::Tmux { .. })) {
        tokio::runtime::Builder::new_current_thread()
    } else {
        tokio::runtime::Builder::new_multi_thread()
    };
    let result = runtime
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(vtamp::cli::run(args)));
    if let Err(error) = result {
        std::process::exit(report(error, json));
    }
}
