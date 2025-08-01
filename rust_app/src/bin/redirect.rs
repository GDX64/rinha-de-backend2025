use axum::extract::connect_info::Connected;
use std::env;
use tokio::io::{self};
use tokio::net::{TcpListener, TcpStream};
use tokio::select;

#[tokio::main]
async fn main() -> io::Result<()> {
    // Set the address to listen on and the target to redirect to
    let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:9999".to_string());
    let target_addrs = env::var("TARGET_ADDRS").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let target_addrs = target_addrs
        .split(',')
        .map(|s| s.to_string())
        .collect::<Vec<String>>();

    let listener = TcpListener::bind(&listen_addr).await?;
    println!("Listening on {}", listen_addr);
    let mut last_chosen: usize = 0;

    // let many = (0..600).map(|_|{

    // })

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
            let (mut ir, mut iw) = inbound.split();
            let (mut or, mut ow) = outbound.split();
            let inbound_to_outbound = tokio::io::copy(&mut ir, &mut ow);
            let outbound_to_inbound = tokio::io::copy(&mut or, &mut iw);
            select! {
                _ = inbound_to_outbound => {}
                _ = outbound_to_inbound => {}
            }
            println!("Connection closed");
        });
    }
}
