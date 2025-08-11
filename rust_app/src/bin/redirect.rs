use std::env;
use std::net::ToSocketAddrs;
use tokio::io::{self};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> io::Result<()> {
    // Set the address to listen on and the target to redirect to
    let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:9999".to_string());
    let target_addrs = env::var("TARGET_ADDRS").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let target_addrs = target_addrs
        .split(',')
        .flat_map(|s| s.to_socket_addrs().unwrap())
        .collect::<Vec<_>>();

    let listener = TcpListener::bind(&listen_addr).await?;

    println!("starting conn pool");
    let mut conns = make_pre_connections(&target_addrs).await;
    println!("conns ready");

    loop {
        let (mut inbound, _) = listener.accept().await?;
        let mut connection = conns.pop().unwrap();

        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut connection).await;
        });
    }
}

async fn make_pre_connections(target_addrs: &[std::net::SocketAddr]) -> Vec<tokio::net::TcpStream> {
    let mut last_chosen = 0;
    let mut connections = Vec::new();
    for _ in 0..700 {
        last_chosen += 1;
        let addr = target_addrs[last_chosen % target_addrs.len()];
        let conn = tokio::net::TcpStream::connect(addr)
            .await
            .expect("Failed to connect to target address");
        connections.push(conn);
    }
    return connections;
}
