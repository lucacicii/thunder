use thunder_std::run_stdio_server;

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    if let Err(e) = run_stdio_server(stdin, stdout).await {
        eprintln!("thunder-std server error: {e}");
        std::process::exit(1);
    }
}
