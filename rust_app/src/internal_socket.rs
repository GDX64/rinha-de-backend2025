use std::io::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::WebSocketStream;

fn create_server(addr: &str, func: impl Fn(WebSocketStream<TcpStream>) + Clone + 'static + Send) {
    let addr = addr.to_string();
    tokio::spawn(async move {
        // Create the event loop and TCP listener we'll accept connections on.
        let try_socket = TcpListener::bind(addr).await;
        let listener = try_socket.expect("Failed to bind");

        while let Ok((stream, _)) = listener.accept().await {
            let _ = stream
                .peer_addr()
                .expect("connected streams should have a peer address");
            let f = func.clone();
            let ws = make_server(stream).await;
            f(ws);
        }
    });
}

async fn make_server<S: AsyncRead + AsyncWrite + Unpin>(stream: S) -> WebSocketStream<S> {
    let ws_stream = tokio_tungstenite::accept_async(stream)
        .await
        .expect("Error during the websocket handshake occurred");
    return ws_stream;
}

async fn make_client(addr: &str) -> WebSocketStream<TcpStream> {
    let stream = TcpStream::connect(addr).await.expect("Failed to connect");
    let (ws_stream, _) = tokio_tungstenite::client_async(format!("ws://{}", addr), stream)
        .await
        .expect("Failed to connect");

    return ws_stream;
}

#[cfg(test)]
mod test {
    use core::time;

    use crate::internal_socket::{create_server, make_client};
    use futures_util::{SinkExt, StreamExt};

    #[tokio::test]
    async fn test() {
        println!("will create client");
        create_server("127.0.0.1:8080", |ws| {
            tokio::spawn(async move {
                let (write, read) = ws.split();
                read.forward(write)
                    .await
                    .expect("Failed to forward messages");
            });
        });
        println!("will create client");
        let ws = make_client("127.0.0.1:8080").await;
        println!("client connected");
        let (mut write, mut read) = ws.split();
        let t2 = tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                let msg = msg.expect("Failed to read message");
                println!("Received: {}", msg);
            }
        });
        let t3 = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                let msg = tokio_tungstenite::tungstenite::Message::Text("Hello".into());
                println!("Sending: Hello");
                if let Err(e) = write.send(msg).await {
                    eprintln!("Failed to send message: {}", e);
                    break;
                }
            }
        });
        let timeout = tokio::time::sleep(time::Duration::from_secs(5));
        timeout.await;
    }
}
