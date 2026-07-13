#[tokio::main]
async fn main() {
    if let Err(error) = fozmo::app::run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
