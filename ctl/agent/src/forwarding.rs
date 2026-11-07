//! One disposable SSH process multiplexes independent remote UDP peer sockets.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use ctl_proto::forwarding::{
  self, Frame, MAX_DATAGRAM_BYTES, MAX_PEERS, PEER_IDLE, Request, Response,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};
use tokio::net::UdpSocket;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::{AbortHandle, JoinSet};

const PEER_QUEUE: usize = 8;
const STREAM_QUEUE: usize = 64;
// Receive every UDP payload whole before applying the published frame bound.
const RECEIVE_BUFFER_BYTES: usize = 65_536;
const SHARED_QUEUED_BYTES: usize = 32 * 1024 * 1024;
const SHARED_PEERS: usize = 512;

/// One endpoint's UDP allocations stay bounded across all logical services.
#[derive(Clone)]
pub(crate) struct UdpBudget {
  bytes: Arc<Semaphore>,
  peers: Arc<Semaphore>,
}

impl UdpBudget {
  pub(crate) fn shared() -> Self {
    Self::new(SHARED_QUEUED_BYTES, SHARED_PEERS)
  }

  fn new(bytes: usize, peers: usize) -> Self {
    Self {
      bytes: Arc::new(Semaphore::new(bytes)),
      peers: Arc::new(Semaphore::new(peers)),
    }
  }

  fn acquire_bytes(&self, size: usize) -> Option<OwnedSemaphorePermit> {
    self
      .bytes
      .clone()
      .try_acquire_many_owned(size.try_into().ok()?)
      .ok()
  }

  fn queue(&self, frame: Frame) -> Option<QueuedFrame> {
    let permit = match &frame {
      Frame::Datagram { payload, .. } => Some(self.acquire_bytes(payload.len())?),
      Frame::Close { .. } => None,
    };
    Some(QueuedFrame {
      frame,
      _permit: permit,
    })
  }
}

struct QueuedFrame {
  frame: Frame,
  // Keep payloads charged while queued and until their actual write completes.
  _permit: Option<OwnedSemaphorePermit>,
}

impl QueuedFrame {
  fn close(peer_id: u64) -> Self {
    Self {
      frame: Frame::Close { peer_id },
      _permit: None,
    }
  }
}

/// Serves this process's SSH descriptors without retaining them after EOF.
///
/// # Errors
/// Returns descriptor, setup, framing, or UDP transport errors.
pub async fn serve_stdio() -> io::Result<()> {
  #[cfg(unix)]
  {
    let (mut reader, mut writer) = ctl_ipc::stdio::take()?;
    serve(&mut reader, &mut writer).await
  }
  #[cfg(not(unix))]
  serve(&mut tokio::io::stdin(), &mut tokio::io::stdout()).await
}

/// Negotiates a forwarding contract, validates a destination, and relays UDP.
///
/// # Errors
/// Returns framing and I/O errors. Destination failures use a structured setup
/// response before any datagram frames are exchanged.
pub async fn serve(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<()> {
  // Standalone forwarding retains its existing per-process queue/peer limits.
  serve_with_budget(
    reader,
    writer,
    UdpBudget::new(Semaphore::MAX_PERMITS, Semaphore::MAX_PERMITS),
  )
  .await
}

pub(crate) async fn serve_with_budget(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
  budget: UdpBudget,
) -> io::Result<()> {
  let socket = tokio::time::timeout(forwarding::STARTUP_TIMEOUT, async {
    writer.write_all(forwarding::PREFACE).await?;
    writer.flush().await?;
    let protocol = forwarding::accept_contract(reader, writer).await?;
    let request: Request = forwarding::read(reader).await?;
    if let Err(error) = request.validate() {
      forwarding::write(
        writer,
        &Response::Error {
          message: error.to_string(),
        },
      )
      .await?;
      return Ok::<_, io::Error>(None);
    }
    let Ok(peer) = budget.peers.clone().try_acquire_owned() else {
      forwarding::write(
        writer,
        &Response::Error {
          message: "UDP forwarding peer limit reached".into(),
        },
      )
      .await?;
      return Ok(None);
    };
    match connect(&request).await {
      Ok(socket) => {
        forwarding::write(writer, &Response::Ready).await?;
        Ok(Some((protocol, socket, peer)))
      }
      Err(error) => {
        forwarding::write(
          writer,
          &Response::Error {
            message: error.to_string(),
          },
        )
        .await?;
        Ok(None)
      }
    }
  })
  .await
  .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UDP forwarding setup timed out"))??;
  let Some((protocol, socket, peer)) = socket else {
    return Ok(());
  };
  if protocol == forwarding::CONTRACT_V1_0_1 {
    let _peer = peer;
    relay(reader, writer, &socket, &budget).await
  } else {
    relay_multiplexed(reader, writer, socket, &budget, peer).await
  }
}

async fn connect(request: &Request) -> io::Result<UdpSocket> {
  let addresses = tokio::net::lookup_host((request.host.as_str(), request.port)).await?;
  let mut last_error = None;
  for address in addresses {
    match connect_address(address).await {
      Ok(socket) => return Ok(socket),
      Err(error) => last_error = Some(error),
    }
  }
  Err(last_error.unwrap_or_else(|| io::Error::other("UDP destination resolved to no addresses")))
}

async fn connect_address(address: SocketAddr) -> io::Result<UdpSocket> {
  let bind = if address.is_ipv6() {
    "[::]:0"
  } else {
    "0.0.0.0:0"
  };
  let socket = UdpSocket::bind(bind).await?;
  socket.connect(address).await?;
  Ok(socket)
}

async fn relay(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
  socket: &UdpSocket,
  budget: &UdpBudget,
) -> io::Result<()> {
  let upload = async {
    while let Some(bytes) = forwarding::read_datagram(reader).await? {
      let Some(_permit) = budget.acquire_bytes(bytes.len()) else {
        continue;
      };
      socket.send(&bytes).await?;
    }
    Ok::<_, io::Error>(())
  };
  let download = async {
    let mut buffer = vec![0; MAX_DATAGRAM_BYTES + 1];
    loop {
      let count = socket.recv(&mut buffer).await?;
      let Some(_permit) = budget.acquire_bytes(count) else {
        continue;
      };
      forwarding::write_datagram(writer, &buffer[..count]).await?;
    }
    #[allow(unreachable_code)]
    Ok::<_, io::Error>(())
  };
  // Keep both complete loops alive: cancelling an individual framed read when
  // a UDP reply arrives could lose a partially consumed header or payload.
  tokio::select! {
    result = upload => result,
    result = download => result,
  }
}

struct Peer {
  sender: mpsc::Sender<QueuedFrame>,
  abort: AbortHandle,
}

async fn relay_multiplexed(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
  socket: UdpSocket,
  budget: &UdpBudget,
  initial_permit: OwnedSemaphorePermit,
) -> io::Result<()> {
  let (input, packets) = mpsc::channel(STREAM_QUEUE);
  let (output, mut replies) = mpsc::channel::<QueuedFrame>(STREAM_QUEUE);
  let receive = async {
    while let Some(packet) = forwarding::read_packet(reader).await? {
      let Some(packet) = budget.queue(packet) else {
        continue;
      };
      if input.send(packet).await.is_err() {
        break;
      }
    }
    Ok(())
  };
  let send = async {
    while let Some(packet) = replies.recv().await {
      forwarding::write_packet(writer, &packet.frame).await?;
    }
    Ok(())
  };
  // Framed reads and writes stay alive as complete loops. A peer reply cannot
  // cancel a partially consumed input frame or interleave output frame bytes.
  tokio::select! {
    result = receive => result,
    result = send => result,
    result = manage_peers(socket, packets, output, budget, initial_permit) => result,
  }
}

async fn manage_peers(
  socket: UdpSocket,
  mut packets: mpsc::Receiver<QueuedFrame>,
  output: mpsc::Sender<QueuedFrame>,
  budget: &UdpBudget,
  initial_permit: OwnedSemaphorePermit,
) -> io::Result<()> {
  let address = socket.peer_addr()?;
  let mut initial = Some((socket, initial_permit));
  let mut highest_peer_id = 0;
  let mut peers: HashMap<u64, Peer> = HashMap::new();
  let mut tasks = JoinSet::new();
  loop {
    tokio::select! {
      biased;
      finished = tasks.join_next(), if !tasks.is_empty() => {
        let peer_id = match finished {
          Some(Ok(peer_id)) => Some(peer_id),
          Some(Err(error)) => peers.iter().find_map(|(&peer_id, peer)| {
            (peer.abort.id() == error.id()).then_some(peer_id)
          }),
          None => None,
        };
        if let Some(peer_id) = peer_id
          && peers.remove(&peer_id).is_some()
          && output.send(QueuedFrame::close(peer_id)).await.is_err()
        {
          break;
        }
      },
      packet = packets.recv() => {
        let Some(packet) = packet else { break; };
        let peer_id = match &packet.frame {
          Frame::Close { peer_id } => {
            if let Some(peer) = peers.remove(peer_id) { peer.abort.abort(); }
            continue;
          },
          Frame::Datagram { peer_id, .. } => *peer_id,
        };
        if let Some(peer) = peers.get(&peer_id) {
          // UDP traffic is lossy under load; one slow destination must not
          // stall every association or allocate an unbounded packet queue.
          let _ = peer.sender.try_send(packet);
          continue;
        }
        // IDs are strictly increasing when an association is created. A
        // delayed frame cannot resurrect a closed peer or reuse its socket.
        if peer_id <= highest_peer_id || peers.len() >= MAX_PEERS {
          highest_peer_id = highest_peer_id.max(peer_id);
          if output.send(QueuedFrame::close(peer_id)).await.is_err() { break; }
          continue;
        }
        highest_peer_id = peer_id;
        let (socket, permit) = if let Some(initial) = initial.take() {
          initial
        } else {
          let Ok(permit) = budget.peers.clone().try_acquire_owned() else {
            if output.send(QueuedFrame::close(peer_id)).await.is_err() { break; }
            continue;
          };
          let Ok(socket) = connect_address(address).await else {
            if output.send(QueuedFrame::close(peer_id)).await.is_err() { break; }
            continue;
          };
          (socket, permit)
        };
        let (sender, receiver) = mpsc::channel(PEER_QUEUE);
        let _ = sender.try_send(packet);
        let output = output.clone();
        let budget = budget.clone();
        let abort = tasks.spawn(async move {
          let _ = relay_peer(peer_id, socket, receiver, &output, &budget, permit).await;
          peer_id
        });
        peers.insert(peer_id, Peer { sender, abort });
      },
    }
  }
  tasks.shutdown().await;
  Ok(())
}

async fn relay_peer(
  peer_id: u64,
  socket: UdpSocket,
  mut packets: mpsc::Receiver<QueuedFrame>,
  output: &mpsc::Sender<QueuedFrame>,
  budget: &UdpBudget,
  _peer_permit: OwnedSemaphorePermit,
) -> io::Result<()> {
  let mut buffer = vec![0; RECEIVE_BUFFER_BYTES];
  let mut deadline = tokio::time::Instant::now() + PEER_IDLE;
  loop {
    tokio::select! {
      () = tokio::time::sleep_until(deadline) => return Ok(()),
      packet = packets.recv() => {
        let Some(packet) = packet else { return Ok(()); };
        if let Frame::Datagram { payload, .. } = &packet.frame {
          socket.send(payload).await?;
        }
        deadline = tokio::time::Instant::now() + PEER_IDLE;
      },
      received = socket.recv(&mut buffer) => {
        let size = received?;
        if size > MAX_DATAGRAM_BYTES { continue; }
        deadline = tokio::time::Instant::now() + PEER_IDLE;
        let Some(packet) = budget.queue(Frame::Datagram {
          peer_id,
          payload: buffer[..size].to_vec(),
        }) else { continue; };
        if matches!(output.try_send(packet), Err(mpsc::error::TrySendError::Closed(_))) {
          return Ok(());
        }
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;
  use tokio::io::AsyncReadExt as _;

  async fn open(
    stream: tokio::io::DuplexStream,
    port: u16,
  ) -> (
    tokio::io::ReadHalf<tokio::io::DuplexStream>,
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
  ) {
    open_for(stream, port, false).await
  }

  async fn open_for(
    stream: tokio::io::DuplexStream,
    port: u16,
    multiplexed: bool,
  ) -> (
    tokio::io::ReadHalf<tokio::io::DuplexStream>,
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
  ) {
    let (reader, writer, response) = request_for(stream, port, multiplexed).await;
    assert!(matches!(response, Response::Ready));
    (reader, writer)
  }

  async fn request_for(
    stream: tokio::io::DuplexStream,
    port: u16,
    multiplexed: bool,
  ) -> (
    tokio::io::ReadHalf<tokio::io::DuplexStream>,
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
    Response,
  ) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut preface = vec![0; forwarding::PREFACE.len()];
    reader.read_exact(&mut preface).await.unwrap();
    assert_eq!(preface, forwarding::PREFACE);
    if multiplexed {
      assert_eq!(
        forwarding::negotiate_multiplex_contract(&mut reader, &mut writer)
          .await
          .unwrap(),
        forwarding::CONTRACT_V1_0_2
      );
    } else {
      let offer: ctl_core::protocol::ProtocolOffer = forwarding::read(&mut reader).await.unwrap();
      assert!(offer.accepts(forwarding::CONTRACT_V1_0_1));
      forwarding::write(
        &mut writer,
        &serde_json::json!({
          "protocol_version": forwarding::CONTRACT_V1_0_1,
        }),
      )
      .await
      .unwrap();
    }
    forwarding::write(
      &mut writer,
      &Request {
        host: "127.0.0.1".into(),
        port,
      },
    )
    .await
    .unwrap();
    let response = forwarding::read::<Response>(&mut reader).await.unwrap();
    (reader, writer, response)
  }

  async fn wait_for_permits(semaphore: &Semaphore, expected: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
      while semaphore.available_permits() != expected {
        tokio::task::yield_now().await;
      }
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn idle_legacy_setup_reserves_shared_socket_capacity_without_affecting_siblings() {
    tokio::time::timeout(Duration::from_secs(5), async {
      let budget = UdpBudget::new(128, 1);
      let destination = UdpSocket::bind("127.0.0.1:0").await.unwrap();
      let port = destination.local_addr().unwrap().port();
      let mut active = None;
      let mut servers = Vec::new();
      for multiplexed in [false, true] {
        let (client, server) = tokio::io::duplex(1024);
        let shared = budget.clone();
        servers.push(tokio::spawn(async move {
          let (mut reader, mut writer) = tokio::io::split(server);
          serve_with_budget(&mut reader, &mut writer, shared)
            .await
            .unwrap();
        }));
        let (reader, writer, response) = request_for(client, port, multiplexed).await;
        if multiplexed {
          assert!(matches!(response, Response::Error { .. }));
        } else {
          assert!(matches!(response, Response::Ready));
          active = Some((reader, writer));
        }
      }
      // Rejecting new work leaves the existing legacy service usable.
      let (mut reader, mut writer) = active.unwrap();
      forwarding::write_datagram(&mut writer, &[9]).await.unwrap();
      let (count, address) = destination.recv_from(&mut [0; 16]).await.unwrap();
      assert_eq!(count, 1);
      destination.send_to(&[7], address).await.unwrap();
      assert_eq!(
        forwarding::read_datagram(&mut reader).await.unwrap(),
        Some(vec![7])
      );
      writer.shutdown().await.unwrap();
      for server in servers {
        server.await.unwrap();
      }
      wait_for_permits(&budget.peers, 1).await;
      wait_for_permits(&budget.bytes, 128).await;
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn shared_budget_bounds_blocked_writes_and_peers_without_blocking_close() {
    tokio::time::timeout(Duration::from_secs(5), async {
      let budget = UdpBudget::new(128, 2);
      let destination = UdpSocket::bind("127.0.0.1:0").await.unwrap();
      let port = destination.local_addr().unwrap().port();
      let mut clients = Vec::new();
      let mut servers = Vec::new();
      let mut addresses = Vec::new();
      for _ in 0..2 {
        let (client, server) = tokio::io::duplex(64);
        let budget = budget.clone();
        servers.push(tokio::spawn(async move {
          let (mut reader, mut writer) = tokio::io::split(server);
          serve_with_budget(&mut reader, &mut writer, budget)
            .await
            .unwrap();
        }));
        let (reader, mut writer) = open_for(client, port, true).await;
        forwarding::write_packet(
          &mut writer,
          &Frame::Datagram {
            peer_id: 1,
            payload: vec![],
          },
        )
        .await
        .unwrap();
        let (count, address) = destination.recv_from(&mut [0; 1]).await.unwrap();
        assert_eq!(count, 0);
        addresses.push(address);
        clients.push((reader, writer));
      }
      wait_for_permits(&budget.peers, 0).await;
      // The first frame exceeds the stream buffer: dequeuing it must keep its
      // payload charged until the blocked frame write finishes.
      destination.send_to(&[7; 128], addresses[0]).await.unwrap();
      wait_for_permits(&budget.bytes, 0).await;
      let (reader, writer) = &mut clients[1];
      for (peer_id, payload) in [(1, vec![9]), (2, vec![])] {
        forwarding::write_packet(writer, &Frame::Datagram { peer_id, payload })
          .await
          .unwrap();
      }
      assert_eq!(
        forwarding::read_packet(reader).await.unwrap(),
        Some(Frame::Close { peer_id: 2 })
      );
      forwarding::write_packet(writer, &Frame::Close { peer_id: 1 })
        .await
        .unwrap();
      wait_for_permits(&budget.peers, 1).await;
      forwarding::write_packet(
        writer,
        &Frame::Datagram {
          peer_id: 3,
          payload: vec![],
        },
      )
      .await
      .unwrap();
      // The nonempty upload was dropped under pressure. The next received
      // packet belongs to the replacement peer, admitted after Close freed it.
      let (count, _) = destination.recv_from(&mut [0; 16]).await.unwrap();
      assert_eq!(count, 0);
      assert_eq!(budget.bytes.available_permits(), 0);
      assert_eq!(
        forwarding::read_packet(&mut clients[0].0).await.unwrap(),
        Some(Frame::Datagram {
          peer_id: 1,
          payload: vec![7; 128]
        })
      );
      wait_for_permits(&budget.bytes, 128).await;
      for (_, writer) in &mut clients {
        writer.shutdown().await.unwrap();
      }
      for server in servers {
        server.await.unwrap();
      }
      wait_for_permits(&budget.peers, 2).await;
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn one_stream_preserves_more_than_ten_independent_udp_peers() {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    let echo = tokio::spawn(async move {
      let mut addresses = std::collections::HashSet::new();
      let mut buffer = [0; 32];
      for _ in 0..MAX_PEERS {
        let (count, address) = echo.recv_from(&mut buffer).await.unwrap();
        assert!(addresses.insert(address));
        echo.send_to(&buffer[..count], address).await.unwrap();
      }
      assert_eq!(addresses.len(), MAX_PEERS);
      let (count, address) = echo.recv_from(&mut buffer).await.unwrap();
      echo.send_to(&buffer[..count], address).await.unwrap();
    });
    let (client, server) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(server);
      serve(&mut reader, &mut writer).await.unwrap();
    });
    let (mut reader, mut writer) = open_for(client, port, true).await;
    let peer_count = u64::try_from(MAX_PEERS).unwrap();
    for peer_id in 1_u64..=peer_count {
      forwarding::write_packet(
        &mut writer,
        &Frame::Datagram {
          peer_id,
          payload: peer_id.to_be_bytes().to_vec(),
        },
      )
      .await
      .unwrap();
    }
    let mut seen = std::collections::HashSet::new();
    for _ in 0..MAX_PEERS {
      let packet =
        tokio::time::timeout(Duration::from_secs(3), forwarding::read_packet(&mut reader))
          .await
          .unwrap()
          .unwrap()
          .unwrap();
      let Frame::Datagram { peer_id, payload } = packet else {
        panic!("peer unexpectedly closed");
      };
      assert_eq!(payload, peer_id.to_be_bytes());
      assert!(seen.insert(peer_id));
    }
    forwarding::write_packet(
      &mut writer,
      &Frame::Datagram {
        peer_id: peer_count + 1,
        payload: vec![1],
      },
    )
    .await
    .unwrap();
    assert_eq!(
      forwarding::read_packet(&mut reader).await.unwrap(),
      Some(Frame::Close {
        peer_id: peer_count + 1
      })
    );
    forwarding::write_packet(&mut writer, &Frame::Close { peer_id: 1 })
      .await
      .unwrap();
    forwarding::write_packet(
      &mut writer,
      &Frame::Datagram {
        peer_id: peer_count + 2,
        payload: vec![2],
      },
    )
    .await
    .unwrap();
    assert_eq!(
      forwarding::read_packet(&mut reader).await.unwrap(),
      Some(Frame::Datagram {
        peer_id: peer_count + 2,
        payload: vec![2]
      })
    );
    writer.shutdown().await.unwrap();
    server.await.unwrap();
    echo.await.unwrap();
  }

  #[tokio::test]
  async fn closing_one_peer_cannot_reuse_it_or_interrupt_other_peers() {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    let (client, server) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(server);
      serve(&mut reader, &mut writer).await.unwrap();
    });
    let (mut reader, mut writer) = open_for(client, port, true).await;
    let mut addresses = Vec::new();
    let mut buffer = [0; 32];
    for peer_id in [1, 2] {
      forwarding::write_packet(
        &mut writer,
        &Frame::Datagram {
          peer_id,
          payload: vec![],
        },
      )
      .await
      .unwrap();
      let (count, address) = echo.recv_from(&mut buffer).await.unwrap();
      assert_eq!(count, 0);
      addresses.push(address);
    }
    forwarding::write_packet(&mut writer, &Frame::Close { peer_id: 1 })
      .await
      .unwrap();
    forwarding::write_packet(
      &mut writer,
      &Frame::Datagram {
        peer_id: 1,
        payload: vec![1],
      },
    )
    .await
    .unwrap();
    assert_eq!(
      forwarding::read_packet(&mut reader).await.unwrap(),
      Some(Frame::Close { peer_id: 1 })
    );
    echo.send_to(&[2], addresses[1]).await.unwrap();
    assert_eq!(
      forwarding::read_packet(&mut reader).await.unwrap(),
      Some(Frame::Datagram {
        peer_id: 2,
        payload: vec![2]
      })
    );
    forwarding::write_packet(
      &mut writer,
      &Frame::Datagram {
        peer_id: 3,
        payload: vec![3],
      },
    )
    .await
    .unwrap();
    let (count, third) = echo.recv_from(&mut buffer).await.unwrap();
    assert_eq!(&buffer[..count], &[3]);
    assert_ne!(third, addresses[1]);
    writer.shutdown().await.unwrap();
    server.await.unwrap();
  }

  #[tokio::test]
  async fn multiplexed_reply_does_not_discard_a_partial_upload_frame() {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    let (client, server) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(server);
      serve(&mut reader, &mut writer).await.unwrap();
    });
    let (mut reader, mut writer) = open_for(client, port, true).await;
    forwarding::write_packet(
      &mut writer,
      &Frame::Datagram {
        peer_id: 1,
        payload: vec![1],
      },
    )
    .await
    .unwrap();
    let mut buffer = [0; 32];
    let (_, address) = echo.recv_from(&mut buffer).await.unwrap();
    let mut bytes = Vec::new();
    forwarding::write_packet(
      &mut bytes,
      &Frame::Datagram {
        peer_id: 1,
        payload: vec![3],
      },
    )
    .await
    .unwrap();
    writer.write_all(&bytes[..5]).await.unwrap();
    writer.flush().await.unwrap();
    echo.send_to(&[2], address).await.unwrap();
    assert_eq!(
      forwarding::read_packet(&mut reader).await.unwrap(),
      Some(Frame::Datagram {
        peer_id: 1,
        payload: vec![2]
      })
    );
    writer.write_all(&bytes[5..]).await.unwrap();
    writer.flush().await.unwrap();
    let (count, _) = tokio::time::timeout(Duration::from_secs(3), echo.recv_from(&mut buffer))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(&buffer[..count], &[3]);
    writer.shutdown().await.unwrap();
    server.await.unwrap();
  }

  #[tokio::test]
  async fn relays_empty_binary_and_large_datagrams_and_stops_on_eof() {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    let echo = tokio::spawn(async move {
      let mut buffer = vec![0; MAX_DATAGRAM_BYTES];
      for _ in 0..3 {
        let (count, address) = echo.recv_from(&mut buffer).await.unwrap();
        echo.send_to(&buffer[..count], address).await.unwrap();
      }
    });
    let (client, server) = tokio::io::duplex(2 * MAX_DATAGRAM_BYTES);
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(server);
      serve(&mut reader, &mut writer).await.unwrap();
    });
    let (mut reader, mut writer) = open(client, port).await;
    // Fits macOS's default UDP send limit; maximum framing is tested in proto.
    for bytes in [vec![], vec![0, 255, 13, 10], vec![123; 8000]] {
      forwarding::write_datagram(&mut writer, &bytes)
        .await
        .unwrap();
      assert_eq!(
        tokio::time::timeout(
          Duration::from_secs(2),
          forwarding::read_datagram(&mut reader)
        )
        .await
        .unwrap()
        .unwrap(),
        Some(bytes)
      );
    }
    writer.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), server)
      .await
      .unwrap()
      .unwrap();
    echo.await.unwrap();
  }

  #[tokio::test]
  async fn two_channels_keep_udp_peers_independent() {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    let mut clients = Vec::new();
    let mut servers = Vec::new();
    for byte in [1, 2] {
      let (client, server) = tokio::io::duplex(4096);
      servers.push(tokio::spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(server);
        serve(&mut reader, &mut writer).await.unwrap();
      }));
      let (reader, mut writer) = open(client, port).await;
      forwarding::write_datagram(&mut writer, &[byte])
        .await
        .unwrap();
      clients.push((reader, writer));
    }
    let mut buffer = [0; 16];
    let (count, first) = echo.recv_from(&mut buffer).await.unwrap();
    assert_eq!(&buffer[..count], &[1]);
    let (count, second) = echo.recv_from(&mut buffer).await.unwrap();
    assert_eq!(&buffer[..count], &[2]);
    assert_ne!(first, second);
    echo.send_to(&[2], second).await.unwrap();
    echo.send_to(&[1], first).await.unwrap();
    for (index, (reader, writer)) in clients.iter_mut().enumerate() {
      assert_eq!(
        forwarding::read_datagram(reader).await.unwrap(),
        Some(vec![u8::try_from(index + 1).unwrap()])
      );
      writer.shutdown().await.unwrap();
    }
    for server in servers {
      server.await.unwrap();
    }
  }

  #[tokio::test]
  async fn a_reply_does_not_discard_a_partially_received_upload_frame() {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    let (client, server) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(server);
      serve(&mut reader, &mut writer).await.unwrap();
    });
    let (mut reader, mut writer) = open(client, port).await;
    forwarding::write_datagram(&mut writer, &[1]).await.unwrap();
    let mut buffer = [0; 16];
    let (_, address) = echo.recv_from(&mut buffer).await.unwrap();
    writer.write_all(&[0, 0]).await.unwrap();
    writer.flush().await.unwrap();
    echo.send_to(&[2], address).await.unwrap();
    assert_eq!(
      forwarding::read_datagram(&mut reader).await.unwrap(),
      Some(vec![2])
    );
    writer.write_all(&[0, 1, 3]).await.unwrap();
    writer.flush().await.unwrap();
    let (count, _) = tokio::time::timeout(Duration::from_secs(2), echo.recv_from(&mut buffer))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(&buffer[..count], &[3]);
    writer.shutdown().await.unwrap();
    server.await.unwrap();
  }

  #[tokio::test]
  async fn rejects_invalid_destination_before_udp_setup() {
    let (client, server) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(server);
      serve(&mut reader, &mut writer).await.unwrap();
    });
    let (mut reader, mut writer) = tokio::io::split(client);
    let mut preface = vec![0; forwarding::PREFACE.len()];
    reader.read_exact(&mut preface).await.unwrap();
    forwarding::negotiate_contract(&mut reader, &mut writer)
      .await
      .unwrap();
    forwarding::write(
      &mut writer,
      &Request {
        host: "localhost;id".into(),
        port: 53,
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      forwarding::read::<Response>(&mut reader).await.unwrap(),
      Response::Error { .. }
    ));
    server.await.unwrap();
  }
}
