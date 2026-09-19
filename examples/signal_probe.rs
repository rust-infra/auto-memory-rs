//! TEMPORARY probe (delete before finishing): does tokio's signal driver deliver a
//! SIGINT sent from outside the process?

use std::time::Duration;

fn main() {
    // Bare tokio runtime: `Runtime::new()` is the multi-thread, all-drivers runtime.
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        println!("probe: armed");
        match tokio::time::timeout(Duration::from_secs(3), tokio::signal::ctrl_c()).await {
            Ok(Ok(())) => println!("probe: SIGINT delivered"),
            Ok(Err(error)) => println!("probe: ctrl_c error: {error}"),
            Err(_) => println!("probe: timed out with no SIGINT"),
        }
    });
}
