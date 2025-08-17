#[cfg(windows)]
mod internal_ws {
    use tokio::{
        io::{AsyncRead, AsyncWrite},
        net::{TcpListener, TcpStream},
    };
    use tokio_tungstenite::WebSocketStream;
    pub type OpaqueWS = WebSocketStream<TcpStream>;

    pub fn create_server(addr: &str, func: impl Fn(OpaqueWS) + Clone + 'static + Send) {
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

    pub async fn make_client(addr: &str) -> OpaqueWS {
        let stream = TcpStream::connect(addr).await.expect("Failed to connect");
        let (ws_stream, _) = tokio_tungstenite::client_async(format!("ws://{}", addr), stream)
            .await
            .expect("Failed to connect");

        return ws_stream;
    }
    #[cfg(test)]
    mod test {
        use super::{create_server, make_client};
        use core::time;
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
            tokio::spawn(async move {
                while let Some(msg) = read.next().await {
                    let msg = msg.expect("Failed to read message");
                    println!("Received: {}", msg);
                }
            });
            tokio::spawn(async move {
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
}

#[cfg(unix)]
mod internal_ws {
    use tokio::{
        io::{AsyncRead, AsyncWrite},
        net::{UnixListener, UnixStream},
    };
    use tokio_tungstenite::WebSocketStream;
    pub type OpaqueWS = WebSocketStream<UnixStream>;

    pub fn create_server(addr: &str, func: impl Fn(OpaqueWS) + Clone + 'static + Send) {
        let addr = addr.to_string();
        tokio::spawn(async move {
            let listener = UnixListener::bind(addr).expect("Failed to bind");

            while let Ok((stream, _)) = listener.accept().await {
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

    pub async fn make_client(addr: &str) -> OpaqueWS {
        let stream = UnixStream::connect(addr).await.expect("Failed to connect");
        let (ws_stream, _) = tokio_tungstenite::client_async("ws://localhost", stream)
            .await
            .expect("Failed to connect");

        return ws_stream;
    }
}

pub use internal_ws::*;
