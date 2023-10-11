
mod server;

#[tokio::main]
async fn main() {
    match server::run("127.0.0.1", 8080).await {
        Ok(()) => println!("Terminating server."),
        Err(e) => println!("Error: {:?}", e),
    }
}
