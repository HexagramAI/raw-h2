use ntex_service::{Service, ServiceCtx};
use ntex_util::channel::mpsc::Sender;
use crate::{Message, Stream, StreamRef};

use std::{fmt, rc::Rc};
use std::task::{Context, Poll};
use ntex_bytes::{ByteString, Bytes};
use ntex_http::{uri::Scheme, HeaderMap, Method};
use ntex_io::{Dispatcher as IoDispatcher, IoBoxed, OnDisconnect};

use crate::connection::Connection;
use crate::default::DefaultControlService;
use crate::dispatcher::Dispatcher;
use crate::{codec::Codec, config::Config, OperationError};
use crate::frame::{Reason, StreamId, WindowSize};

#[derive(Clone)]
pub struct SimpleClient(Rc<ClientRef>);

/// Http2 client
struct ClientRef {
  con: Connection,
  authority: ByteString,
  message_tx: Sender<Message>,
}

impl SimpleClient {
  /// Construct new `Client` instance.
  pub fn new<T>(io: T, config: Config, scheme: Scheme, authority: ByteString, message_tx: Sender<Message>) -> Self
  where
    IoBoxed: From<T>,
  {
    SimpleClient::with_params(
      io.into(),
      config,
      scheme,
      authority,
      message_tx,
    )
  }

  pub(super) fn with_params(
    io: IoBoxed,
    config: Config,
    scheme: Scheme,
    authority: ByteString,
    message_tx: Sender<Message>,
  ) -> Self {
    let codec = Codec::default();
    let con = Connection::new(io.get_ref(), codec, config, false);
    con.set_secure(scheme == Scheme::HTTPS);

    let disp = Dispatcher::new(
      con.clone(),
      DefaultControlService,
      HandleService::new(message_tx.clone()),
    );

    let fut = IoDispatcher::new(
      io,
      con.codec().clone(),
      disp,
      &con.config().dispatcher_config,
    );
    let _ = ntex_util::spawn(async move {
      let _ = fut.await;
    });

    SimpleClient(Rc::new(ClientRef {
      con,
      authority,
      message_tx,
    }))
  }

  #[inline]
  /// Send request to the peer
  pub async fn send(
    &self,
    method: Method,
    path: ByteString,
    headers: HeaderMap,
    eof: bool,
  ) -> Result<SendStream, OperationError> {
    let stream = self
      .0
      .con
      .send_request(self.0.authority.clone(), method, path, headers, eof)
      .await?;

    Ok(SendStream(stream))
  }

  #[inline]
  /// Check if client is allowed to send new request
  ///
  /// Readiness depends on number of opened streams and max concurrency setting
  pub fn is_ready(&self) -> bool {
    self.0.con.can_create_new_stream()
  }

  #[inline]
  /// Check client readiness
  ///
  /// Client is ready when it is possible to start new stream
  pub async fn ready(&self) -> Result<(), OperationError> {
    self.0.con.ready().await
  }

  #[inline]
  /// Gracefully close connection
  pub fn close(&self) {
    log::debug!("Closing client");
    self.0.con.disconnect_when_ready()
  }

  #[inline]
  /// Close connection
  pub fn force_close(&self) {
    self.0.con.close()
  }

  #[inline]
  /// Check if connection is closed
  pub fn is_closed(&self) -> bool {
    self.0.con.is_closed()
  }

  #[inline]
  /// Notify when connection get closed
  pub fn on_disconnect(&self) -> OnDisconnect {
    self.0.con.io().on_disconnect()
  }

  #[inline]
  /// Client's authority
  pub fn authority(&self) -> &ByteString {
    &self.0.authority
  }

  /// Get max number of active streams
  pub fn max_streams(&self) -> Option<u32> {
    self.0.con.max_streams()
  }

  /// Get number of active streams
  pub fn active_streams(&self) -> u32 {
    self.0.con.active_streams()
  }
}

impl Drop for SimpleClient {
  fn drop(&mut self) {
    if Rc::strong_count(&self.0) == 1 {
      log::debug!("Last h2 client has been dropped, disconnecting");
      self.0.con.disconnect_when_ready()
    }
  }
}

impl fmt::Debug for SimpleClient {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ntex_h2::SimpleClient")
      .field("authority", &self.0.authority)
      .field("connection", &self.0.con)
      .finish()
  }
}

#[derive(Debug)]
/// Send part of the client stream
pub struct SendStream(Stream);

impl Drop for SendStream {
  fn drop(&mut self) {
    // if !self.0.send_state().is_closed() {
    //   self.0.reset(Reason::CANCEL);
    // }
  }
}

impl SendStream {
  #[inline]
  pub fn id(&self) -> StreamId {
    self.0.id()
  }

  #[inline]
  pub fn stream(&self) -> &StreamRef {
    &self.0
  }

  #[inline]
  pub fn available_send_capacity(&self) -> WindowSize {
    self.0.available_send_capacity()
  }

  #[inline]
  pub async fn send_capacity(&self) -> Result<WindowSize, OperationError> {
    self.0.send_capacity().await
  }

  #[inline]
  /// Send payload
  pub async fn send_payload(&self, res: Bytes, eof: bool) -> Result<(), OperationError> {
    self.0.send_payload(res, eof).await
  }

  #[inline]
  pub fn send_trailers(&self, map: HeaderMap) {
    self.0.send_trailers(map)
  }

  #[inline]
  /// Reset stream
  pub fn reset(&self, reason: Reason) {
    self.0.reset(reason)
  }

  #[inline]
  /// Check for available send capacity
  pub fn poll_send_capacity(&self, cx: &Context<'_>) -> Poll<Result<WindowSize, OperationError>> {
    self.0.poll_send_capacity(cx)
  }

  #[inline]
  /// Check if send part of stream get reset
  pub fn poll_send_reset(&self, cx: &Context<'_>) -> Poll<Result<(), OperationError>> {
    self.0.poll_send_reset(cx)
  }
}


pub(super) struct HandleService(Sender<Message>);

impl HandleService {
  pub(super) fn new(message_tx: Sender<Message>) -> Self {
    Self(message_tx)
  }
}

impl Service<Message> for HandleService {
  type Response = ();
  type Error = ();

  async fn call(&self, msg: Message, _: ServiceCtx<'_, Self>) -> Result<(), ()> {
    // println!("#call# got message {:?}", msg);
    self.0.send(msg).expect("Could not send message to client");
    Ok(())
  }
}