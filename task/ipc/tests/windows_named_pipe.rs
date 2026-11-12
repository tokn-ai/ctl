#![cfg(windows)]

use ctl_task_ipc::{connect, windows::bind};
use interprocess::local_socket::traits::tokio::Listener as _;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::task::JoinSet;
use tokio::time::timeout;

// Exercise the application's actual transport while the I/O driver and peers
// run on different workers. Mio must retain submitted I/O until its completion,
// even when the peer disconnects immediately: tokio-rs/mio#2014.
#[test]
fn named_pipe_disconnects_and_runtime_shutdown_preserve_io_lifetimes() {
  for _ in 0..3 {
    let runtime = tokio::runtime::Builder::new_multi_thread()
      .worker_threads(4)
      .enable_all()
      .build()
      .unwrap();
    runtime.block_on(async {
      timeout(Duration::from_secs(30), async {
        let mut lanes = JoinSet::new();
        for _ in 0..4 {
          lanes.spawn(exercise_connections());
        }
        while let Some(result) = lanes.join_next().await {
          result.unwrap();
        }
      })
      .await
      .expect("named pipe lifecycle stalled");
    });
    // Short-lived CLI processes tear down the runtime after dropping streams.
    drop(runtime);
  }
}

async fn exercise_connections() {
  let endpoint = PathBuf::from(format!(
    r"\\.\pipe\ctl-task-ipc-test-{}",
    uuid::Uuid::new_v4()
  ));
  let listener = bind(&endpoint).unwrap();
  for round in 0..128 {
    let (client, server) = tokio::join!(connect(&endpoint), listener.accept());
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    match round % 4 {
      0 => {
        // Reading the response resubmits a read as the peer closes its handle.
        let request = [42];
        let ((), ()) = tokio::join!(
          async {
            let mut received = [0];
            server.read_exact(&mut received).await.unwrap();
            assert_eq!(received, request);
            server.write_all(&received).await.unwrap();
            drop(server);
          },
          async {
            client.write_all(&request).await.unwrap();
            let mut received = Vec::new();
            client.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, request);
          }
        );
      }
      1 => {
        let ((), result) = tokio::join!(
          async {
            tokio::task::yield_now().await;
            drop(server);
          },
          client.read_u8()
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
      }
      2 => {
        // A write larger than the pipe buffer races a peer that stops reading.
        let data = vec![7; 128 * 1024];
        let ((), written) = tokio::join!(
          async {
            tokio::task::yield_now().await;
            drop(server);
          },
          client.write_all(&data)
        );
        if let Err(error) = written {
          assert!(
            matches!(
              error.kind(),
              io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::NotConnected
            ),
            "unexpected write error: {error}"
          );
        }
        let mut received = Vec::new();
        client.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, Vec::<u8>::new());
      }
      _ => {
        // Cancelling a pending read must leave its backing I/O alive until the
        // driver observes cancellation, including when the stream is dropped.
        assert!(
          timeout(Duration::from_millis(1), client.read_u8())
            .await
            .is_err()
        );
        drop(client);
        let mut received = Vec::new();
        server.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, Vec::<u8>::new());
      }
    }
    tokio::task::yield_now().await;
  }
}
