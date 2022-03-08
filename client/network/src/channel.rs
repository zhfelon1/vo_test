use crate::api::NetworkConnectError;
use async_trait::async_trait;
use bytes::BytesMut;
use futures_util::FutureExt;
use network_protocol::{
    Bandwidth, Cid, InitProtocolError, MpscMsg, MpscRecvProtocol, MpscSendProtocol, Pid,
    ProtocolError, ProtocolEvent, ProtocolMetricCache, ProtocolMetrics, Sid, TcpRecvProtocol,
    TcpSendProtocol, UnreliableDrain, UnreliableSink,
};
use hashbrown::HashMap;
use std::{
    io,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net,
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
    select,
    sync::{mpsc, oneshot, Mutex},
};
use tracing::{error, info, trace, warn};

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum Protocols {
    Tcp((TcpSendProtocol<TcpDrain>, TcpRecvProtocol<TcpSink>)),
    Mpsc((MpscSendProtocol<MpscDrain>, MpscRecvProtocol<MpscSink>)),
}

#[derive(Debug)]
pub(crate) enum SendProtocols {
    Tcp(TcpSendProtocol<TcpDrain>),
    Mpsc(MpscSendProtocol<MpscDrain>),
}

#[derive(Debug)]
pub(crate) enum RecvProtocols {
    Tcp(TcpRecvProtocol<TcpSink>),
    Mpsc(MpscRecvProtocol<MpscSink>),
}

lazy_static::lazy_static! {
    pub(crate) static ref MPSC_POOL: Mutex<HashMap<u64, mpsc::UnboundedSender<C2cMpscConnect>>> = {
        Mutex::new(HashMap::new())
    };
}

pub(crate) type C2cMpscConnect = (
    mpsc::Sender<MpscMsg>,
    oneshot::Sender<mpsc::Sender<MpscMsg>>,
);

impl Protocols {
    const MPSC_CHANNEL_BOUND: usize = 1000;

    pub(crate) async fn with_tcp_connect(
        addr: SocketAddr,
        metrics: ProtocolMetricCache,
    ) -> Result<Self, NetworkConnectError> {
        let stream = net::TcpStream::connect(addr)
            .await
            .and_then(|s| {
                s.set_nodelay(true)?;
                Ok(s)
            })
            .map_err(NetworkConnectError::Io)?;
        info!(
            "Connecting Tcp to: {}",
            stream.peer_addr().map_err(NetworkConnectError::Io)?
        );
        Ok(Self::new_tcp(stream, metrics))
    }

    pub(crate) async fn with_tcp_listen(
        addr: SocketAddr,
        cids: Arc<AtomicU64>,
        metrics: Arc<ProtocolMetrics>,
        s2s_stop_listening_r: oneshot::Receiver<()>,
        c2s_protocol_s: mpsc::UnboundedSender<(Self, Cid)>,
    ) -> std::io::Result<()> {

        use socket2::{Domain, Socket, Type};
        let domain = Domain::for_address(addr);
        let socket2_socket = Socket::new(domain, Type::STREAM, None)?;
        if domain == Domain::IPV6 {
            socket2_socket.set_only_v6(true)?
        }
        socket2_socket.set_nonblocking(true)?; // Needed by Tokio
        // See https://docs.rs/tokio/latest/tokio/net/struct.TcpSocket.html
        #[cfg(not(windows))]
        socket2_socket.set_reuse_address(true)?;
        let socket2_addr = addr.into();
        socket2_socket.bind(&socket2_addr)?;
        socket2_socket.listen(1024)?;
        let std_listener: std::net::TcpListener = socket2_socket.into();
        let listener = tokio::net::TcpListener::from_std(std_listener)?;
        trace!(?addr, "Tcp Listener bound");
        let mut end_receiver = s2s_stop_listening_r.fuse();
        tokio::spawn(async move {
            while let Some(data) = select! {
                    next = listener.accept().fuse() => Some(next),
                    _ = &mut end_receiver => None,
            } {
                let (stream, remote_addr) = match data {
                    Ok((s, p)) => (s, p),
                    Err(e) => {
                        trace!(?e, "TcpStream Error, ignoring connection attempt");
                        continue;
                    },
                };
                if let Err(e) = stream.set_nodelay(true) {
                    warn!(
                        ?e,
                        "Failed to set TCP_NODELAY, client may have degraded latency"
                    );
                }
                let cid = cids.fetch_add(1, Ordering::Relaxed);
                info!(?remote_addr, ?cid, "Accepting Tcp from");
                let metrics = ProtocolMetricCache::new(&cid.to_string(), Arc::clone(&metrics));
                let _ = c2s_protocol_s.send((Self::new_tcp(stream, metrics.clone()), cid));
            }
        });
        Ok(())
    }

    pub(crate) fn new_tcp(stream: tokio::net::TcpStream, metrics: ProtocolMetricCache) -> Self {
        let (r, w) = stream.into_split();
        let sp = TcpSendProtocol::new(TcpDrain { half: w }, metrics.clone());
        let rp = TcpRecvProtocol::new(
            TcpSink {
                half: r,
                buffer: BytesMut::new(),
            },
            metrics,
        );
        Protocols::Tcp((sp, rp))
    }

    pub(crate) async fn with_mpsc_connect(
        addr: u64,
        metrics: ProtocolMetricCache,
    ) -> Result<Self, NetworkConnectError> {
        let mpsc_s = MPSC_POOL
            .lock()
            .await
            .get(&addr)
            .ok_or_else(|| {
                NetworkConnectError::Io(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "no mpsc listen on this addr",
                ))
            })?
            .clone();
        let (remote_to_local_s, remote_to_local_r) = mpsc::channel(Self::MPSC_CHANNEL_BOUND);
        let (local_to_remote_oneshot_s, local_to_remote_oneshot_r) = oneshot::channel();
        mpsc_s
            .send((remote_to_local_s, local_to_remote_oneshot_s))
            .map_err(|_| {
                NetworkConnectError::Io(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "mpsc pipe broke during connect",
                ))
            })?;
        let local_to_remote_s = local_to_remote_oneshot_r
            .await
            .map_err(|e| NetworkConnectError::Io(io::Error::new(io::ErrorKind::BrokenPipe, e)))?;
        info!(?addr, "Connecting Mpsc");
        Ok(Self::new_mpsc(
            local_to_remote_s,
            remote_to_local_r,
            metrics,
        ))
    }

    pub(crate) async fn with_mpsc_listen(
        addr: u64,
        cids: Arc<AtomicU64>,
        metrics: Arc<ProtocolMetrics>,
        s2s_stop_listening_r: oneshot::Receiver<()>,
        c2s_protocol_s: mpsc::UnboundedSender<(Self, Cid)>,
    ) -> std::io::Result<()> {
        let (mpsc_s, mut mpsc_r) = mpsc::unbounded_channel();
        MPSC_POOL.lock().await.insert(addr, mpsc_s);
        trace!(?addr, "Mpsc Listener bound");
        let mut end_receiver = s2s_stop_listening_r.fuse();
        tokio::spawn(async move {
            while let Some((local_to_remote_s, local_remote_to_local_s)) = select! {
                    next = mpsc_r.recv().fuse() => next,
                    _ = &mut end_receiver => None,
            } {
                let (remote_to_local_s, remote_to_local_r) =
                    mpsc::channel(Self::MPSC_CHANNEL_BOUND);
                if let Err(e) = local_remote_to_local_s.send(remote_to_local_s) {
                    error!(?e, "mpsc listen aborted");
                }

                let cid = cids.fetch_add(1, Ordering::Relaxed);
                info!(?addr, ?cid, "Accepting Mpsc from");
                let metrics = ProtocolMetricCache::new(&cid.to_string(), Arc::clone(&metrics));
                let _ = c2s_protocol_s.send((
                    Self::new_mpsc(local_to_remote_s, remote_to_local_r, metrics.clone()),
                    cid,
                ));
            }
            warn!("MpscStream Failed, stopping");
        });
        Ok(())
    }

    pub(crate) fn new_mpsc(
        sender: mpsc::Sender<MpscMsg>,
        receiver: mpsc::Receiver<MpscMsg>,
        metrics: ProtocolMetricCache,
    ) -> Self {
        let sp = MpscSendProtocol::new(MpscDrain { sender }, metrics.clone());
        let rp = MpscRecvProtocol::new(MpscSink { receiver }, metrics);
        Protocols::Mpsc((sp, rp))
    }


    pub(crate) fn split(self) -> (SendProtocols, RecvProtocols) {
        match self {
            Protocols::Tcp((s, r)) => (SendProtocols::Tcp(s), RecvProtocols::Tcp(r)),
            Protocols::Mpsc((s, r)) => (SendProtocols::Mpsc(s), RecvProtocols::Mpsc(r)),
        }
    }
}

#[async_trait]
impl network_protocol::InitProtocol for Protocols {
    async fn initialize(
        &mut self,
        initializer: bool,
        local_pid: Pid,
        secret: u128,
    ) -> Result<(Pid, Sid, u128), InitProtocolError> {
        match self {
            Protocols::Tcp(p) => p.initialize(initializer, local_pid, secret).await,
            Protocols::Mpsc(p) => p.initialize(initializer, local_pid, secret).await,
        }
    }
}

#[async_trait]
impl network_protocol::SendProtocol for SendProtocols {
    fn notify_from_recv(&mut self, event: ProtocolEvent) {
        match self {
            SendProtocols::Tcp(s) => s.notify_from_recv(event),
            SendProtocols::Mpsc(s) => s.notify_from_recv(event),
        }
    }

    async fn send(&mut self, event: ProtocolEvent) -> Result<(), ProtocolError> {
        match self {
            SendProtocols::Tcp(s) => s.send(event).await,
            SendProtocols::Mpsc(s) => s.send(event).await,
        }
    }

    async fn flush(
        &mut self,
        bandwidth: Bandwidth,
        dt: Duration,
    ) -> Result<Bandwidth, ProtocolError> {
        match self {
            SendProtocols::Tcp(s) => s.flush(bandwidth, dt).await,
            SendProtocols::Mpsc(s) => s.flush(bandwidth, dt).await,
        }
    }
}

#[async_trait]
impl network_protocol::RecvProtocol for RecvProtocols {
    async fn recv(&mut self) -> Result<ProtocolEvent, ProtocolError> {
        match self {
            RecvProtocols::Tcp(r) => r.recv().await,
            RecvProtocols::Mpsc(r) => r.recv().await,
        }
    }
}

///////////////////////////////////////
//// TCP
#[derive(Debug)]
pub struct TcpDrain {
    half: OwnedWriteHalf,
}

#[derive(Debug)]
pub struct TcpSink {
    half: OwnedReadHalf,
    buffer: BytesMut,
}

#[async_trait]
impl UnreliableDrain for TcpDrain {
    type DataFormat = BytesMut;

    async fn send(&mut self, data: Self::DataFormat) -> Result<(), ProtocolError> {
        match self.half.write_all(&data).await {
            Ok(()) => Ok(()),
            Err(_) => Err(ProtocolError::Closed),
        }
    }
}

#[async_trait]
impl UnreliableSink for TcpSink {
    type DataFormat = BytesMut;

    async fn recv(&mut self) -> Result<Self::DataFormat, ProtocolError> {
        self.buffer.resize(1500, 0u8);
        match self.half.read(&mut self.buffer).await {
            Ok(0) => Err(ProtocolError::Closed),
            Ok(n) => Ok(self.buffer.split_to(n)),
            Err(_) => Err(ProtocolError::Closed),
        }
    }
}

///////////////////////////////////////
//// MPSC
#[derive(Debug)]
pub struct MpscDrain {
    sender: tokio::sync::mpsc::Sender<MpscMsg>,
}

#[derive(Debug)]
pub struct MpscSink {
    receiver: tokio::sync::mpsc::Receiver<MpscMsg>,
}

#[async_trait]
impl UnreliableDrain for MpscDrain {
    type DataFormat = MpscMsg;

    async fn send(&mut self, data: Self::DataFormat) -> Result<(), ProtocolError> {
        self.sender
            .send(data)
            .await
            .map_err(|_| ProtocolError::Closed)
    }
}

#[async_trait]
impl UnreliableSink for MpscSink {
    type DataFormat = MpscMsg;

    async fn recv(&mut self) -> Result<Self::DataFormat, ProtocolError> {
        self.receiver.recv().await.ok_or(ProtocolError::Closed)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use network_protocol::{Promises, ProtocolMetrics, RecvProtocol, SendProtocol};
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn tokio_sinks() {
        let listener = TcpListener::bind("127.0.0.1:5000").await.unwrap();
        let r1 = tokio::spawn(async move {
            let (server, _) = listener.accept().await.unwrap();
            (listener, server)
        });
        let client = TcpStream::connect("127.0.0.1:5000").await.unwrap();
        let (_listener, server) = r1.await.unwrap();
        let metrics = ProtocolMetricCache::new("0", Arc::new(ProtocolMetrics::new().unwrap()));
        let client = Protocols::new_tcp(client, metrics.clone());
        let server = Protocols::new_tcp(server, metrics);
        let (mut s, _) = client.split();
        let (_, mut r) = server.split();
        let event = ProtocolEvent::OpenStream {
            sid: Sid::new(1),
            prio: 4u8,
            promises: Promises::GUARANTEED_DELIVERY,
            guaranteed_bandwidth: 1_000,
        };
        s.send(event.clone()).await.unwrap();
        s.send(ProtocolEvent::Message {
            sid: Sid::new(1),
            data: Bytes::from(&[8u8; 8][..]),
        })
        .await
        .unwrap();
        s.flush(1_000_000, Duration::from_secs(1)).await.unwrap();
        drop(s); // recv must work even after shutdown of send!
        tokio::time::sleep(Duration::from_secs(1)).await;
        let res = r.recv().await;
        match res {
            Ok(ProtocolEvent::OpenStream {
                sid,
                prio,
                promises,
                guaranteed_bandwidth: _,
            }) => {
                assert_eq!(sid, Sid::new(1));
                assert_eq!(prio, 4u8);
                assert_eq!(promises, Promises::GUARANTEED_DELIVERY);
            },
            _ => {
                panic!("wrong type {:?}", res);
            },
        }
        r.recv().await.unwrap();
    }

    #[tokio::test]
    async fn tokio_sink_stop_after_drop() {
        let listener = TcpListener::bind("127.0.0.1:5001").await.unwrap();
        let r1 = tokio::spawn(async move {
            let (server, _) = listener.accept().await.unwrap();
            (listener, server)
        });
        let client = TcpStream::connect("127.0.0.1:5001").await.unwrap();
        let (_listener, server) = r1.await.unwrap();
        let metrics = ProtocolMetricCache::new("0", Arc::new(ProtocolMetrics::new().unwrap()));
        let client = Protocols::new_tcp(client, metrics.clone());
        let server = Protocols::new_tcp(server, metrics);
        let (s, _) = client.split();
        let (_, mut r) = server.split();
        let e = tokio::spawn(async move { r.recv().await });
        drop(s);
        let e = e.await.unwrap();
        assert!(e.is_err());
        assert_eq!(e.unwrap_err(), ProtocolError::Closed);
    }
}
