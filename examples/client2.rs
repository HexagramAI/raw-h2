use std::error::Error;
use std::net::SocketAddr;
use ntex::rt::tcp_connect;
use ntex_bytes::Bytes;
use raw_h2::{Config, MessageKind};
use ntex_http::{header, uri::Scheme, HeaderMap, Method};
use ntex_util::time::{sleep, Seconds};
use raw_h2::client::simple2::SimpleClient;

#[ntex::main]
pub async fn main() -> Result<(), Box<dyn Error>> {
  // std::env::set_var("RUST_LOG", "trace,polling=info,mio=info");
  env_logger::init();

  let addr: SocketAddr = "127.0.0.1:5928".parse().unwrap();
  let io = tcp_connect(addr).await?;
  let (msg_tx, msg_rx) = ntex_util::channel::mpsc::channel();
  let config = Config::client();
  let client = SimpleClient::new(io, config, Scheme::HTTP, "example.com".into(), msg_tx);

  ntex::rt::spawn(async move {
    while let Some(msg) = msg_rx.recv().await {
      let stream_id: u32 = msg.stream.id().into();
      println!("got stream_id={} {:?}", stream_id, msg);
      match msg.kind {
        MessageKind::Headers { .. } => {}
        MessageKind::Data(_, _) => {}
        MessageKind::Eof(data) => {
          println!("stream_id={} done", stream_id);
        },
        MessageKind::Disconnect(e) => {
          break;
        },
      }
    }
    println!("done");
  });

  let mut hdrs = HeaderMap::default();
  hdrs.insert(
    header::CONTENT_TYPE,
    header::HeaderValue::try_from("text/plain").unwrap(),
  );
  let stream = client
    .send(Method::GET, "/".into(), hdrs, false)
    .await
    .unwrap();

  stream
    .send_payload(Bytes::from_static(b"testing"), true)
    .await
    .unwrap();

  sleep(Seconds(20)).await;
  Ok(())
}
