use crate::api::NetworkConnectError;
use async_trait::async_trait;
use bytes::BytesMut;

use tokio::sync::{mpsc, oneshot};



#[cfg(not(target_arch = "wasm32"))]
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(not(target_arch = "wasm32"))]
use tokio::net;

#[cfg(not(target_arch = "wasm32"))]
use tokio::net:: tcp::{OwnedReadHalf, OwnedWriteHalf};

#[cfg(not(target_arch = "wasm32"))]
use tokio::select;

#[cfg(not(target_arch = "wasm32"))]
use futures_util::FutureExt;

use network_protocol::{
    Bandwidth, Cid, InitProtocolError, Pid,
    ProtocolError, ProtocolEvent, Sid, TcpRecvProtocol,
    TcpSendProtocol, UnreliableDrain, UnreliableSink,
};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use instant::Duration;

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum Protocols {
    Tcp((TcpSendProtocol<TcpDrain>, TcpRecvProtocol<TcpSink>)),
}

#[derive(Debug)]
pub(crate) enum SendProtocols {
    Tcp(TcpSendProtocol<TcpDrain>),
}

#[derive(Debug)]
pub(crate) enum RecvProtocols {
    Tcp(TcpRecvProtocol<TcpSink>),
}

impl Protocols {

    pub(crate) async fn with_tcp_connect(
        addr: SocketAddr,
    ) -> Result<Self, NetworkConnectError> {

        //tcp连接
        #[cfg(not(target_arch = "wasm32"))]
        {
            let stream = net::TcpStream::connect(addr)
                .await
                .and_then(|s| {
                    s.set_nodelay(true)?;
                    Ok(s)
                })
                .map_err(NetworkConnectError::Io)?;
                log::info!(
                "Connecting Tcp to: {}",
                stream.peer_addr().map_err(NetworkConnectError::Io)?
            );
            Ok(Self::new_tcp(stream))
        }
    
        //websocket连接 todo
        #[cfg(target_arch = "wasm32")]
        {
            log::error!("########## todo with_tcp_connect");
            Err(NetworkConnectError::InvalidSecret)
        }
    }

    pub(crate) async fn with_tcp_listen(
        addr: SocketAddr,
        cids: Arc<AtomicU64>,
        s2s_stop_listening_r: oneshot::Receiver<()>,
        c2s_protocol_s: mpsc::UnboundedSender<(Self, Cid)>,
    ) -> std::io::Result<()> {

        //tcp连接
        #[cfg(not(target_arch = "wasm32"))]
        {
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
            log::trace!("Tcp Listener bound {}", addr);
            let mut end_receiver = s2s_stop_listening_r.fuse();
            tokio::spawn(async move {
                while let Some(data) = select! {
                        next = listener.accept().fuse() => Some(next),
                        _ = &mut end_receiver => None,
                } {
                    let (stream, remote_addr) = match data {
                        Ok((s, p)) => (s, p),
                        Err(e) => {
                            log::trace!("TcpStream Error, ignoring connection attempt {:?}", &e);
                            continue;
                        },
                    };
                    if let Err(e) = stream.set_nodelay(true) {
                        log::warn!(
                            "Failed to set TCP_NODELAY, client may have degraded latency  {:?}", &e
                        );
                    }
                    let cid = cids.fetch_add(1, Ordering::Relaxed);
                    log::info!("Accepting Tcp from, {}, {}", remote_addr, cid);
                }
            });
        }
    
        //websocket连接 todo
        #[cfg(target_arch = "wasm32")]
        {
            log::error!("########## todo with tcp listen");
        }

        Ok(())
    }

    //tcp连接
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn new_tcp(stream: tokio::net::TcpStream) -> Self {

        let (r, w) = stream.into_split();
        let sp = TcpSendProtocol::new(TcpDrain { half: w });
        let rp = TcpRecvProtocol::new(
            TcpSink {
                half: r,
                buffer: BytesMut::new(),
            },
        );
        Protocols::Tcp((sp, rp))
    }
   

    pub(crate) fn split(self) -> (SendProtocols, RecvProtocols) {
        match self {
            Protocols::Tcp((s, r)) => (SendProtocols::Tcp(s), RecvProtocols::Tcp(r)),
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
        }
    }
}

#[async_trait]
impl network_protocol::SendProtocol for SendProtocols {
    fn notify_from_recv(&mut self, event: ProtocolEvent) {
        match self {
            SendProtocols::Tcp(s) => s.notify_from_recv(event),
        }
    }

    async fn send(&mut self, event: ProtocolEvent) -> Result<(), ProtocolError> {
        match self {
            SendProtocols::Tcp(s) => s.send(event).await,
        }
    }

    async fn flush(
        &mut self,
        bandwidth: Bandwidth,
        dt: Duration,
    ) -> Result<Bandwidth, ProtocolError> {
        match self {
            SendProtocols::Tcp(s) => s.flush(bandwidth, dt).await,
        }
    }
}

#[async_trait]
impl network_protocol::RecvProtocol for RecvProtocols {
    async fn recv(&mut self) -> Result<ProtocolEvent, ProtocolError> {
        match self {
            RecvProtocols::Tcp(r) => r.recv().await,
        }
    }
}

///////////////////////////////////////
//// TCP
#[derive(Debug)]
pub struct TcpDrain {
    #[cfg(not(target_arch = "wasm32"))]
    half: OwnedWriteHalf,
}

#[derive(Debug)]
pub struct TcpSink {
    #[cfg(not(target_arch = "wasm32"))]
    half: OwnedReadHalf,
    
    buffer: BytesMut,
}

#[async_trait]
impl UnreliableDrain for TcpDrain {
    type DataFormat = BytesMut;

    async fn send(&mut self, data: Self::DataFormat) -> Result<(), ProtocolError> {
       
        //tcp连接
        #[cfg(not(target_arch = "wasm32"))]
        {
            match self.half.write_all(&data).await {
                Ok(()) => Ok(()),
                Err(_) => Err(ProtocolError::Closed),
            }
        }
    
        //websocket连接 todo
        #[cfg(target_arch = "wasm32")]
        {
            log::error!("########## todo UnreliableDrain for TcpDrain send");
            Ok(())
        }
    }
}

#[async_trait]
impl UnreliableSink for TcpSink {
    type DataFormat = BytesMut;

    async fn recv(&mut self) -> Result<Self::DataFormat, ProtocolError> {

        //tcp连接
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.buffer.resize(1500, 0u8);
            match self.half.read(&mut self.buffer).await {
                Ok(0) => Err(ProtocolError::Closed),
                Ok(n) => Ok(self.buffer.split_to(n)),
                Err(_) => Err(ProtocolError::Closed),
            }
        }
    
        //websocket连接 todo
        #[cfg(target_arch = "wasm32")]
        {
            log::error!("########## todo impl UnreliableSink for TcpSink recv");
            Err(ProtocolError::Closed)
        }
    }
}
