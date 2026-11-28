use super::*;
use crate::daemon::transport::server;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

#[tokio::test]
async fn a_switch_retries_only_the_input_record_that_the_predecessor_never_consumed() {
  // Small physical pipes force a partial write while the predecessor parks.
  let (mut peer, mut relay) = aio::duplex(16);
  let (mut host, local) = aio::duplex(1024);
  let mut input = BufReader::new(local);
  let mut pending = Vec::new();
  let mut assembling = Vec::new();
  let mut request = vec![b'x'; 256];
  request.push(b'\n');
  host.write_all(&request).await.unwrap();
  let mut sink = aio::sink();
  let (first, _) = timeout(Duration::from_secs(2), async {
    tokio::join!(
      epoch(
        &mut relay,
        &mut input,
        &mut sink,
        &mut pending,
        &mut assembling
      ),
      async {
        // Read the header, then send SWITCH with zero admitted records while
        // the rest of the client's pipe write is still backpressured.
        assert_eq!(peer.read_u32_le().await.unwrap(), request.len() as u32);
        let (read, write) = aio::split(&mut peer);
        let (reader, mut writer) = server(read, write);
        writer.switch(reader.consumed()).await.unwrap()
      }
    )
  })
  .await
  .unwrap();
  assert!(first.unwrap());
  assert_eq!(pending, request);

  let (peer, mut relay) = aio::duplex(16);
  let mut output = Vec::new();
  let (second, _) = tokio::join!(
    epoch(
      &mut relay,
      &mut input,
      &mut output,
      &mut pending,
      &mut assembling
    ),
    async {
      let (read, write) = aio::split(peer);
      let (mut reader, mut writer) = server(read, write);
      let mut got = Vec::new();
      reader.read_until(b'\n', &mut got).await.unwrap();
      assert_eq!(got, request);
      writer.write_all(b"receipt\n").await.unwrap();
      writer.switch(reader.consumed()).await.unwrap();
    }
  );
  assert!(second.unwrap());
  assert!(
    pending.is_empty(),
    "a confirmed action must never be resent"
  );
  assert_eq!(output, b"receipt\n");
}

#[tokio::test]
async fn an_ordinary_disconnect_never_authorizes_resending_an_unacknowledged_action() {
  let (mut peer, mut relay) = aio::duplex(64);
  let (mut host, local) = aio::duplex(64);
  host.write_all(b"click\n").await.unwrap();
  let mut input = BufReader::new(local);
  let mut pending = Vec::new();
  let mut assembling = Vec::new();
  let mut output = Vec::new();
  let (result, _) = tokio::join!(
    epoch(
      &mut relay,
      &mut input,
      &mut output,
      &mut pending,
      &mut assembling
    ),
    async move {
      assert_eq!(peer.read_u32_le().await.unwrap(), 6);
      let mut bytes = [0u8; 6];
      peer.read_exact(&mut bytes).await.unwrap();
      assert_eq!(&bytes, b"click\n");
      drop(peer);
    }
  );
  assert!(!result.unwrap());
  assert!(output.is_empty());
}
