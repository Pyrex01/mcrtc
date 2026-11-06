//! The shared TCP <-> data channel pump.
//!
//! A data channel here is a reliable, ordered byte stream, so chunks go out in the order they
//! were read and the receiver writes them to TCP as they arrive. One bridge is a single future
//! with no spawned tasks, so when it returns nothing is left running.

use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch, Notify};
use tokio::time::{sleep, timeout, Instant};
use tracing::{debug, warn};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelState};

pub const MAX_CHUNK: usize = 16 * 1024;
pub const BUFFERED_LOW: usize = 256 * 1024;
pub const BUFFERED_HIGH: usize = 1024 * 1024;

/// Messages waiting for the TCP writer. When this fills up the event pump stops polling, the
/// library's own queue fills behind it, and SCTP finally tells the remote side to slow down.
const INBOUND_QUEUE: usize = 16;

/// A stalled TCP peer has to be cut off, not waited for. The library stops draining the whole
/// connection once one channel's event queue is full, so a stuck socket would otherwise freeze
/// every other stream of that friend, and their heartbeat with them.
const WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for the remote side to acknowledge the last bytes before closing.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(15);

/// See [`wait_for_capacity`].
const CAPACITY_RECHECK: Duration = Duration::from_millis(20);

const FLUSH_RECHECK: Duration = Duration::from_millis(5);

#[derive(Clone, Default)]
pub struct Counter(Arc<watch::Sender<usize>>);

/// Holds one slot of a [`Counter`] until dropped.
pub struct Slot(Counter);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0 .0.send_modify(|count| *count -= 1);
    }
}

impl Counter {
    pub fn get(&self) -> usize {
        *self.0.borrow()
    }

    pub fn acquire(&self) -> Slot {
        self.0.send_modify(|count| *count += 1);
        Slot(self.clone())
    }

    /// Takes a slot unless `limit` are already in use.
    pub fn try_acquire(&self, limit: Option<usize>) -> Option<Slot> {
        let acquired = self.0.send_if_modified(|count| {
            let free = limit.is_none_or(|limit| *count < limit);
            if free {
                *count += 1;
            }
            free
        });
        acquired.then(|| Slot(self.clone()))
    }

    pub fn subscribe(&self) -> watch::Receiver<usize> {
        self.0.subscribe()
    }

    /// Resolves once fewer than `limit` slots are in use.
    pub async fn wait_below(&self, limit: usize) {
        let mut count = self.0.subscribe();
        let _ = count.wait_for(|count| *count < limit).await;
    }
}

#[derive(Default)]
struct Signals {
    open: watch::Sender<bool>,
    buffered_low: Notify,
}

/// Pumps bytes both ways until either side closes, then closes the other.
pub async fn bridge(mut stream: TcpStream, channel: Arc<dyn DataChannel>) {
    if let Err(error) = stream.set_nodelay(true) {
        warn!(%error, "could not set TCP_NODELAY");
    }

    let (mut tcp_in, mut tcp_out) = stream.split();
    let (queue_tx, queue_rx) = mpsc::channel(INBOUND_QUEUE);
    let signals = Signals::default();

    let inbound = async {
        let pump = pump_events(channel.as_ref(), queue_tx, &signals);
        let writer = write_tcp(queue_rx, &mut tcp_out);
        tokio::pin!(pump, writer);
        tokio::select! {
            // The channel closed. The writer drains what is queued, then sees the queue end.
            () = &mut pump => writer.await,
            // The writer only finishes first when the socket failed.
            result = &mut writer => result,
        }
    };
    let outbound = read_tcp(&mut tcp_in, channel.as_ref(), &signals);

    tokio::select! {
        result = inbound => debug!(?result, "bridge ended by the data channel side"),
        result = outbound => debug!(?result, "bridge ended by the TCP side"),
    }

    if let Err(error) = channel.close().await {
        debug!(%error, "data channel was already closed");
    }
}

/// The only consumer of the channel's events. Returns when the channel is gone.
async fn pump_events(channel: &dyn DataChannel, queue: mpsc::Sender<BytesMut>, signals: &Signals) {
    while let Some(event) = channel.poll().await {
        match event {
            DataChannelEvent::OnOpen => {
                signals.open.send_replace(true);
            }
            DataChannelEvent::OnMessage(message) => {
                if queue.send(message.data).await.is_err() {
                    return;
                }
            }
            DataChannelEvent::OnBufferedAmountLow => signals.buffered_low.notify_one(),
            DataChannelEvent::OnClose => return,
            _ => {}
        }
    }
}

async fn write_tcp(
    mut queue: mpsc::Receiver<BytesMut>,
    tcp: &mut WriteHalf<'_>,
) -> std::io::Result<()> {
    while let Some(data) = queue.recv().await {
        timeout(WRITE_STALL_TIMEOUT, tcp.write_all(&data)).await??;
    }
    tcp.shutdown().await
}

async fn read_tcp(
    tcp: &mut ReadHalf<'_>,
    channel: &dyn DataChannel,
    signals: &Signals,
) -> anyhow::Result<()> {
    let mut open = signals.open.subscribe();
    if channel.ready_state().await? != RTCDataChannelState::Open {
        open.wait_for(|open| *open).await?;
    }
    channel
        .set_buffered_amount_low_threshold(BUFFERED_LOW as u32)
        .await?;

    let mut buffer = vec![0; MAX_CHUNK];
    loop {
        wait_for_capacity(channel, signals).await?;
        let read = tcp.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        channel.send(BytesMut::from(&buffer[..read])).await?;
    }
    flush(channel).await
}

/// Holds TCP reads back while too much is unacknowledged.
///
/// Resuming normally rides on the buffered-amount-low event. That event shares a queue with
/// incoming messages, though, so it can sit behind them for as long as the TCP writer is slow.
/// Echo-style traffic would then deadlock: the writer waits for the remote side to read, which
/// waits for this side to send. The timed recheck reads the counter directly, which needs no
/// event, so the two directions can never wait on each other.
async fn wait_for_capacity(channel: &dyn DataChannel, signals: &Signals) -> anyhow::Result<()> {
    if channel.outstanding_bytes().await? <= BUFFERED_HIGH {
        return Ok(());
    }
    loop {
        tokio::select! {
            () = signals.buffered_low.notified() => {}
            () = sleep(CAPACITY_RECHECK) => {}
        }
        if channel.outstanding_bytes().await? <= BUFFERED_LOW {
            return Ok(());
        }
    }
}

/// Waits until the remote side has everything, because closing may abandon what is queued. The
/// last bytes matter: a server's disconnect message is usually followed directly by its FIN.
async fn flush(channel: &dyn DataChannel) -> anyhow::Result<()> {
    let deadline = Instant::now() + FLUSH_TIMEOUT;
    while channel.outstanding_bytes().await? > 0 {
        if Instant::now() >= deadline {
            warn!("gave up waiting for the remote side to acknowledge the last bytes");
            break;
        }
        sleep(FLUSH_RECHECK).await;
    }
    Ok(())
}
