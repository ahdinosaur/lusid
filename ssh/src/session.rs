//! Async SSH channel and session abstractions.
//!
//! Provides:
//! - AsyncSession: thin wrapper around russh::client::Handle with convenience
//!   connect and open_channel methods.
//! - AsyncChannel: wrapper around russh::Channel with async stdout/stderr
//!   streams, stdin writer, and event promises (success/failure, EOF, exit
//!   status). Also exposes a wait_close() method.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use async_promise::Promise;
use russh::client::{Config, Handle, Handler, Msg, connect};
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, ssh_key};
use russh::{ChannelMsg, ChannelWriteHalf, CryptoVec, Error as SshError};
use tokio::io::AsyncWrite;
use tokio::net::ToSocketAddrs;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::stream::ReadStream;

/// A handler that does NOT check the server's public key.
///
/// Only use in controlled environments with public key authentication.
pub struct NoCheckHandler;

impl Handler for NoCheckHandler {
    type Error = SshError;

    async fn check_server_key(
        &mut self,
        _server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// An SSH session that can open multiple AsyncChannels.
///
/// Implements Deref to the underlying russh::client::Handle.
pub struct AsyncSession<H: Handler> {
    session: Handle<H>,
}

impl<H: 'static + Handler> AsyncSession<H> {
    /// Connect to an SSH server using the provided configuration and handler,
    /// without beginning authentication.
    pub async fn connect(
        config: Arc<Config>,
        addrs: impl ToSocketAddrs,
        handler: H,
    ) -> Result<Self, H::Error> {
        let session = connect(config, addrs, handler).await?;
        Ok(Self { session })
    }

    /// Open an asynchronous channel in this session.
    pub async fn open_channel(&self) -> Result<AsyncChannel, SshError> {
        let russh_channel = self.session.channel_open_session().await?;
        Ok(AsyncChannel::from(russh_channel))
    }
}

impl AsyncSession<NoCheckHandler> {
    /// Connect and authenticate with the given user and key_path via public key.
    ///
    /// Uses NoCheckHandler (skips host key verification).
    pub async fn auth_publickey(
        &mut self,
        username: impl AsRef<str>,
        private_key: PrivateKey,
    ) -> Result<(), SshError> {
        let hash_alg = self.best_supported_rsa_hash().await?.flatten();
        let auth = self
            .authenticate_publickey(
                username.as_ref(),
                PrivateKeyWithHashAlg::new(Arc::new(private_key), hash_alg),
            )
            .await?;

        if !auth.success() {
            tracing::warn!("SSH authentication failed");
            return Err(SshError::NotAuthenticated);
        }

        tracing::info!("SSH authentication successful");
        Ok(())
    }
}

impl<H: Handler> Deref for AsyncSession<H> {
    type Target = Handle<H>;
    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl<H: Handler> DerefMut for AsyncSession<H> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}

/// An asynchronous SSH channel with ReadStream stdout/stderr, AsyncWrite stdin,
/// and event promises for exec success/failure, EOF, and exit status.
///
/// Implements Deref to the underlying ChannelWriteHalf.
pub struct AsyncChannel {
    write_half: ChannelWriteHalf<Msg>,
    subscribe_send: mpsc::UnboundedSender<(Option<u32>, mpsc::UnboundedSender<CryptoVec>)>,
    success_failure: Promise<bool>,
    eof: Promise<()>,
    exit_status: Promise<u32>,
    reader: JoinHandle<()>,
}

impl From<russh::Channel<Msg>> for AsyncChannel {
    fn from(inner: russh::Channel<Msg>) -> Self {
        let (mut read_half, write_half) = inner.split();
        let (mut resolve_success_failure, success_failure) = async_promise::channel();
        let (mut resolve_eof, eof) = async_promise::channel();
        let (mut resolve_exit_status, exit_status) = async_promise::channel();
        let (subscribe_send, mut subscribe_recv) = mpsc::unbounded_channel();

        let reader = async move {
            // Map from `ext` to a sender for CryptoVecs of data.
            type Subscribers = HashMap<Option<u32>, mpsc::UnboundedSender<CryptoVec>>;
            let mut subscribers = Some(Subscribers::new());

            #[tracing::instrument(level = "INFO", skip_all, fields(?ext))]
            fn receive_data(subscribers: &Option<Subscribers>, ext: Option<u32>, data: CryptoVec) {
                if let Some(subscribers) = subscribers {
                    if let Some(send) = subscribers.get(&ext) {
                        if let Err(e) = send.send(data) {
                            tracing::warn!("Failed to send data to subscriber: {e}");
                        } else {
                            tracing::debug!("Successfully sent data to subscriber.");
                        }
                    } else {
                        tracing::debug!("No subscriber for ext, dropping data.");
                    }
                } else {
                    tracing::warn!("Unexpectedly received data from server after receiving EOF.");
                }
            }

            loop {
                tokio::select! {
                    biased;

                    Some((ext, send)) = subscribe_recv.recv() => {
                        if let Some(subscribers) = &mut subscribers {
                            subscribers.insert(ext, send);
                        } else {
                            tracing::debug!(ext, "Received stream subscriber after EOF, ignoring.");
                        }
                    },

                    opt_msg = read_half.wait() => {
                        let Some(msg) = opt_msg else {
                            break;
                        };

                        tracing::info_span!("Message", ?msg).in_scope(|| {
                            match msg {
                                ChannelMsg::Data { data } => {
                                    receive_data(&subscribers, None, data)
                                }
                                ChannelMsg::ExtendedData { data, ext } => {
                                    receive_data(&subscribers, Some(ext), data)
                                }
                                ChannelMsg::Success | ChannelMsg::Failure => {
                                    tracing::debug!("Resolving success/failure.");
                                    let is_success = matches!(msg, ChannelMsg::Success);
                                    if resolve_success_failure.resolve(is_success).is_err() {
                                        tracing::warn!(
                                            "Success/failure already resolved, ignoring."
                                        );
                                    }
                                }
                                ChannelMsg::Eof => {
                                    tracing::debug!(
                                        "Resolving EOF and dropping stream subscribers."
                                    );
                                    if resolve_eof.resolve(()).is_err() {
                                        tracing::warn!("EOF already resolved, ignoring.");
                                    }
                                    drop(std::mem::take(&mut subscribers));
                                }
                                ChannelMsg::ExitStatus { exit_status } => {
                                    tracing::debug!(exit_status, "Resolving exit status.");
                                    if resolve_exit_status.resolve(exit_status).is_err() {
                                        tracing::warn!(
                                            "Exit status already resolved, ignoring."
                                        );
                                    }
                                }
                                _ => {
                                    tracing::trace!("Ignoring message.");
                                }
                            }
                        });
                    },
                }
            }

            tracing::debug!("Channel read half finished, reader exiting.");
        };

        let reader = tokio::task::spawn(reader.instrument(tracing::info_span!("Reader")));

        Self {
            write_half,
            subscribe_send,
            success_failure,
            eof,
            exit_status,
            reader,
        }
    }
}

impl AsyncChannel {
    /// Returns the specified stream as a ReadStream.
    ///
    /// Call this before exec so output isn't missed. Re-calling for the same
    /// ext replaces the previous subscriber.
    pub fn read_stream(&self, ext: Option<u32>) -> ReadStream {
        let (send, recv) = mpsc::unbounded_channel();
        let _ = self.subscribe_send.send((ext, send));
        ReadStream::from_recv(recv)
    }

    /// Returns stdout as a ReadStream.
    pub fn stdout(&self) -> ReadStream {
        self.read_stream(None)
    }

    /// Returns stderr as a ReadStream.
    pub fn stderr(&self) -> ReadStream {
        self.read_stream(Some(1))
    }

    /// Returns the specified stream as an AsyncWrite.
    pub fn write_stream(&self, ext: Option<u32>) -> impl AsyncWrite + use<> {
        self.write_half.make_writer_ext(ext)
    }

    /// Returns stdin as an AsyncWrite.
    pub fn stdin(&self) -> impl AsyncWrite + use<> {
        self.write_stream(None)
    }

    /// Resolves when success or failure has been received.
    pub fn recv_success_failure(&self) -> &Promise<bool> {
        &self.success_failure
    }

    /// Resolves when EOF has been received (no more stdout/stderr).
    pub fn recv_eof(&self) -> &Promise<()> {
        &self.eof
    }

    /// Resolves when the command exit status has been received.
    pub fn recv_exit_status(&self) -> &Promise<u32> {
        &self.exit_status
    }

    /// Await until the channel has been closed.
    pub async fn wait_close(&mut self) {
        let _ = (&mut self.reader).await;
    }

    /// Whether the channel has been closed.
    pub fn is_closed(&self) -> bool {
        self.reader.is_finished()
    }

    /// Change the window size
    pub async fn window_change(
        &self,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
    ) -> Result<(), SshError> {
        self.write_half
            .window_change(col_width, row_height, pix_width, pix_height)
            .await
    }
}

impl Deref for AsyncChannel {
    type Target = ChannelWriteHalf<Msg>;
    fn deref(&self) -> &Self::Target {
        &self.write_half
    }
}
