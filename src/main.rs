use clap::Parser;
use vtamp::cli::{Args, report};

#[tokio::main]
async fn main() {
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
    if let Err(error) = vtamp::cli::run(args).await {
        std::process::exit(report(error, json));
    }
}
