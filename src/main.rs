use server::run;

mod server;

#[tokio::main]
async fn main() {
    // TODO
    run().await;

    println!("Hello, world!");
}
