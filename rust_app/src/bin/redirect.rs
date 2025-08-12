use std::env;
use std::net::ToSocketAddrs;
use tokio::net::{TcpListener, TcpStream};

async fn main_async() -> anyhow::Result<()> {
    // Set the address to listen on and the target to redirect to
    let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:9999".to_string());
    let target_addrs = env::var("TARGET_ADDRS").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let target_addrs = target_addrs
        .split(',')
        .flat_map(|s| s.to_socket_addrs().unwrap())
        .collect::<Vec<_>>();

    let listener = TcpListener::bind(&listen_addr).await?;
    let mut last_chosen: usize = 0;

    loop {
        let (mut inbound, _) = listener.accept().await?;
        let target_addr = target_addrs[last_chosen].clone();
        last_chosen = (last_chosen + 1) % target_addrs.len();

        tokio::spawn(async move {
            let connection = TcpStream::connect(target_addr).await;
            let mut outbound = match connection {
                Ok(outbound) => outbound,
                Err(e) => {
                    eprintln!("Failed to connect to target: {}", e);
                    return ();
                }
            };
            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
        });
    }
}

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .event_interval(1)
        .build()
        .expect("Failed to create Tokio runtime");
    return rt.block_on(main_async());
}
