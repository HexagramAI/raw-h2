use std::{cell::Cell, io, net, rc::Rc};

use ::openssl::ssl::{AlpnError, SslAcceptor, SslConnector, SslFiletype, SslMethod, SslVerifyMode};
use ntex::http::{
    test::server as test_server, uri::Scheme, HeaderMap, HttpService, Method, Response,
};
use ntex::service::{fn_service, ServiceFactory};
use ntex::time::{sleep, Millis};
use ntex::{channel::oneshot, connect::openssl, io::IoBoxed, util::Bytes};
use ntex_h2::{client, client::Client, client::SimpleClient, frame::Reason};

fn ssl_acceptor() -> SslAcceptor {
    // load ssl keys
    let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
    builder
        .set_private_key_file("./tests/key.pem", SslFiletype::PEM)
        .unwrap();
    builder
        .set_certificate_chain_file("./tests/cert.pem")
        .unwrap();
    builder.set_alpn_select_callback(|_, protos| {
        const H2: &[u8] = b"\x02h2";
        const H11: &[u8] = b"\x08http/1.1";
        if protos.windows(3).any(|window| window == H2) {
            Ok(b"h2")
        } else if protos.windows(9).any(|window| window == H11) {
            Ok(b"http/1.1")
        } else {
            Err(AlpnError::NOACK)
        }
    });
    builder
        .set_alpn_protos(b"\x08http/1.1\x02h2")
        .expect("Cannot contrust SslAcceptor");

    builder.build()
}

fn start_server() -> ntex::http::test::TestServer {
    test_server(move || {
        HttpService::build()
            .configure_http2(|cfg| {
                cfg.max_concurrent_streams(1);
            })
            .h2(|mut req: ntex::http::Request| async move {
                let mut pl = req.take_payload();
                pl.recv().await;
                Ok::<_, io::Error>(Response::Ok().body("test body"))
            })
            .openssl(ssl_acceptor())
            .map_err(|_| ())
    })
}

async fn connect(addr: net::SocketAddr) -> IoBoxed {
    // disable ssl verification
    let mut builder = SslConnector::builder(SslMethod::tls()).unwrap();
    builder.set_verify(SslVerifyMode::NONE);
    let _ = builder
        .set_alpn_protos(b"\x02h2\x08http/1.1")
        .map_err(|e| log::error!("Cannot set alpn protocol: {:?}", e));

    let addr = ntex::connect::Connect::new("localhost").set_addr(Some(addr));
    openssl::Connector::new(builder.build())
        .connect(addr)
        .await
        .unwrap()
        .into()
}

#[ntex::test]
async fn test_max_concurrent_streams() {
    let srv = start_server();
    let addr = srv.addr();
    let client = client::Connector::new(fn_service(move |_| {
        let addr = addr;
        async move { Ok(connect(addr).await) }
    }))
    .scheme(Scheme::HTTP)
    .connector(fn_service(move |_| {
        let addr = addr;
        async move { Ok(connect(addr).await) }
    }))
    .connect("localhost")
    .await
    .unwrap();

    loop {
        sleep(Millis(150)).await; // we need to get settings frame from server
        if client.max_streams() == Some(1) {
            break;
        }
    }

    let (stream, _recv_stream) = client
        .send(Method::GET, "/".into(), HeaderMap::default(), false)
        .await
        .unwrap();
    assert!(!client.is_ready());
    assert!(client.active_streams() == 1);

    let client2 = client.clone();
    let opened = Rc::new(Cell::new(false));
    let opened2 = opened.clone();
    ntex::rt::spawn(async move {
        let _stream = client2
            .send(Method::GET, "/".into(), HeaderMap::default(), false)
            .await
            .unwrap();
        opened2.set(true);
    });

    stream.send_payload(Bytes::new(), true).await.unwrap();
    sleep(Millis(50)).await;
    assert!(client.is_ready());
    assert!(opened.get());
}

#[ntex::test]
async fn test_max_concurrent_streams_pool() {
    env_logger::init();
    let srv = start_server();
    let addr = srv.addr();
    let client = Client::build(
        "localhost",
        fn_service(move |_| {
            let addr = addr;
            async move { Ok(connect(addr).await) }
        }),
    )
    .maxconn(1)
    .finish();
    assert!(client.is_ready());

    let (stream, _recv_stream) = client
        .send(Method::GET, "/".into(), HeaderMap::default(), false)
        .await
        .unwrap();
    sleep(Millis(250)).await;
    assert!(!client.is_ready());

    let client2 = client.clone();
    let opened = Rc::new(Cell::new(false));
    let opened2 = opened.clone();
    ntex::rt::spawn(async move {
        let _stream = client2
            .send(Method::GET, "/".into(), HeaderMap::default(), false)
            .await
            .unwrap();
        opened2.set(true);
    });

    stream.send_payload(Bytes::new(), true).await.unwrap();
    client.ready().await;
    sleep(Millis(150)).await;
    assert!(client.is_ready());
    assert!(opened.get());
}

#[ntex::test]
async fn test_max_concurrent_streams_pool2() {
    let srv = start_server();
    let addr = srv.addr();

    let cnt = Rc::new(Cell::new(0));
    let cnt2 = cnt.clone();
    let client = Client::build(
        "localhost",
        fn_service(move |_| {
            let addr = addr;
            cnt2.set(cnt2.get() + 1);
            async move { Ok(connect(addr).await) }
        }),
    )
    .maxconn(2)
    .finish();
    assert!(client.is_ready());

    let (stream, _recv_stream) = client
        .send(Method::GET, "/".into(), HeaderMap::default(), false)
        .await
        .unwrap();
    sleep(Millis(250)).await;
    assert!(client.is_ready());

    let client2 = client.clone();
    let opened = Rc::new(Cell::new(false));
    let opened2 = opened.clone();
    ntex::rt::spawn(async move {
        let _stream = client2
            .send(Method::GET, "/".into(), HeaderMap::default(), false)
            .await
            .unwrap();
        opened2.set(true);
    });

    stream.send_payload(Bytes::new(), true).await.unwrap();
    sleep(Millis(250)).await;
    assert!(client.is_ready());
    assert!(opened.get());
    assert!(cnt.get() == 2);
}

#[ntex::test]
async fn test_max_concurrent_streams_reset() {
    let srv = start_server();
    let io = connect(srv.addr()).await;
    let client = SimpleClient::new(
        io,
        ntex_h2::Config::client(),
        Scheme::HTTP,
        "localhost".into(),
    );
    sleep(Millis(150)).await;

    let (stream, _recv_stream) = client
        .send(Method::GET, "/".into(), HeaderMap::default(), false)
        .await
        .unwrap();
    assert!(!client.is_ready());

    let opened = Rc::new(Cell::new(0));

    let client2 = client.clone();
    let opened2 = opened.clone();
    ntex::rt::spawn(async move {
        let (_stream, _recv_stream) = client2
            .send(Method::GET, "/".into(), HeaderMap::default(), false)
            .await
            .unwrap();
        _stream.reset(Reason::NO_ERROR);
        opened2.set(opened2.get() + 1);
    });
    let client2 = client.clone();
    let opened2 = opened.clone();
    ntex::rt::spawn(async move {
        let _stream = client2
            .send(Method::GET, "/".into(), HeaderMap::default(), false)
            .await
            .unwrap();
        drop(_stream);
        opened2.set(opened2.get() + 1);
    });
    let client2 = client.clone();
    let opened2 = opened.clone();
    let (tx, rx) = oneshot::channel();
    ntex::rt::spawn(async move {
        let _stream = client2
            .send(Method::GET, "/".into(), HeaderMap::default(), false)
            .await
            .unwrap();
        opened2.set(opened2.get() + 1);
        let _ = tx.send(());
    });
    sleep(Millis(50)).await;

    stream.send_payload("chunk".into(), false).await.unwrap();
    sleep(Millis(25)).await;
    stream.reset(Reason::NO_ERROR);
    let _ = rx.await;
    assert!(client.is_ready());
    assert_eq!(opened.get(), 3);
}
