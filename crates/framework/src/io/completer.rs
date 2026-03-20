//! Thread 3: response completer relay.
//!
//! Bridges crossbeam (Thread 2) → tokio oneshot (Thread 1). Blocks on
//! `crossbeam_channel::Receiver::recv()` and fires the tokio oneshot
//! sender for each response. This is a dedicated OS thread — blocking
//! recv is fine.

use super::channel::OutboundChannel;

/// Spawn the completer thread that relays responses from Thread 2 to Thread 1.
///
/// Returns the `JoinHandle` so the caller can join on shutdown.
///
/// # Errors
///
/// Returns an IO error if the thread cannot be spawned.
pub fn spawn(outbound: &OutboundChannel) -> std::io::Result<std::thread::JoinHandle<()>> {
    let rx = outbound.receiver().clone();
    std::thread::Builder::new()
        .name("apx-completer".to_owned())
        .spawn(move || {
            while let Ok(slot) = rx.recv() {
                let _ = slot.completer.send(slot.response);
            }
        })
}
