//! TEMPORARY probe (delete before finishing): does tokio's signal driver deliver
//! SIGINT inside this environment, and does it survive a runtime built by
//! `runtime::executor`?

use std::time::Duration;

use auto_memory::runtime::block_on;

fn ping_self() {
    let pid = std::process::id().to_string();
    let status = std::process::Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .expect("kill");
    eprintln!("probe: kill -INT {pid} -> {status}");
}

#[test]
fn ctrl_c_is_delivered_on_the_project_runtime() {
    let sender = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(300));
        ping_self();
    });
    let delivered = block_on(async {
        tokio::time::timeout(Duration::from_secs(3), tokio::signal::ctrl_c()).await
    })
    .expect("runtime");
    sender.join().expect("sender");
    match delivered {
        Ok(Ok(())) => eprintln!("probe: SIGINT delivered"),
        Ok(Err(error)) => panic!("probe: ctrl_c errored: {error}"),
        Err(_) => panic!("probe: ctrl_c timed out, signal not delivered"),
    }
}
