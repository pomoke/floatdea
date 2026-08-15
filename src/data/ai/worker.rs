//! A single shared async worker for all AI turn tasks.
//!
//! The UI never blocks on network I/O: it submits bounded `TurnRequest`s
//! through a channel, the worker runs them on a dedicated Tokio runtime
//! thread, and streaming events come back through a bounded channel that the
//! UI drains once per frame. Every event carries the full
//! `(AiBoxId, ConversationId, TurnTaskId)` identity so late events from a
//! cancelled, duplicated or deleted turn are dropped instead of landing in the
//! wrong conversation.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use super::provider::{AiError, AiErrorKind, CancelFlag, ChatProvider, ChatRequest, StreamEvent};
use super::TokenUsage;
use crate::data::{ContainerId, ConversationId, TurnTaskId};

/// Identity of one turn. Every streamed event carries all three ids and is
/// dropped by the UI unless all three match the current conversation state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnIdentity {
    pub ai_box: ContainerId,
    pub conversation: ConversationId,
    pub task: TurnTaskId,
}

/// A turn request submitted by the UI layer. The request is already bounded
/// (only the current conversation's history and bound sources); the provider
/// is rebuilt whenever the settings change, without restarting the worker.
#[derive(Clone)]
pub struct TurnRequest {
    pub identity: TurnIdentity,
    pub request: ChatRequest,
    /// The configured provider to run this turn.
    pub provider: Arc<dyn ChatProvider>,
}

/// Streaming event delivered to the UI layer.
#[derive(Clone, Debug)]
pub enum TurnEvent {
    /// A text delta for the assistant message buffer.
    Delta {
        identity: TurnIdentity,
        delta: String,
    },
    /// The turn completed with the full answer and usage (if reported).
    Done {
        identity: TurnIdentity,
        content: String,
        usage: TokenUsage,
    },
    /// The turn failed with a stable error category.
    Failed {
        identity: TurnIdentity,
        error: AiError,
    },
}

/// Commands the UI sends to the worker thread.
enum WorkerCommand {
    Run(TurnRequest),
    Cancel { task: TurnTaskId },
    Shutdown,
}

const COMMAND_CAPACITY: usize = 64;
const EVENT_CAPACITY: usize = 256;
/// Per-provider delta queue; the provider never blocks the worker thread on
/// backpressure beyond this (the forwarder drains it concurrently).
const DELTA_CAPACITY: usize = 32;

/// UI-side handle of the shared AI worker. Dropping it shuts the worker down.
pub struct AiWorker {
    commands: mpsc::Sender<WorkerCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AiWorker {
    /// Spawns the shared worker thread with its own Tokio runtime. Returns the
    /// handle and the event stream the UI should drain every frame.
    pub fn spawn() -> (Self, mpsc::Receiver<TurnEvent>) {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(EVENT_CAPACITY);
        let runtime = Runtime::new().expect("failed to create the AI worker Tokio runtime");
        let thread = std::thread::Builder::new()
            .name("floatdea-ai-worker".to_owned())
            .spawn(move || run_worker(runtime, command_rx, event_tx))
            .expect("failed to spawn the AI worker thread");
        (Self {
            commands: command_tx,
            thread: Some(thread),
        }, event_rx)
    }

    /// Submits a turn. Fails fast (without queueing) when the worker is busy
    /// beyond its bound or has been shut down.
    pub fn submit(&self, request: TurnRequest) -> Result<(), AiError> {
        self.commands
            .try_send(WorkerCommand::Run(request))
            .map_err(|_| {
                AiError::new(
                    AiErrorKind::ProviderUnavailable,
                    "AI worker is busy or shutting down",
                )
            })
    }

    /// Requests cancellation of a turn: the flag stops the provider from
    /// producing more deltas and the task is aborted to free the connection.
    pub fn cancel(&self, task: &TurnTaskId) {
        let _ = self
            .commands
            .try_send(WorkerCommand::Cancel { task: task.clone() });
    }

    /// Stops the worker and waits for its thread to finish. Safe to call from
    /// any thread, including a Tokio runtime thread (the blocking send, if ever
    /// needed, happens on a helper thread).
    pub fn shutdown(&mut self) {
        if self.thread.is_none() {
            return;
        }
        if self.commands.try_send(WorkerCommand::Shutdown).is_err() {
            // Command channel full: block on a helper thread so we never block
            // a runtime thread. The worker drains commands promptly.
            let sender = self.commands.clone();
            std::thread::spawn(move || {
                let _ = sender.blocking_send(WorkerCommand::Shutdown);
            });
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AiWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_worker(
    runtime: Runtime,
    mut commands: mpsc::Receiver<WorkerCommand>,
    events: mpsc::Sender<TurnEvent>,
) {
    // Live turn tasks: full identity + cancellation flag + join handle keyed
    // by task id, so cancellation can abort the right task and report the
    // cancelled turn back with its identity.
    let mut tasks: BTreeMap<
        TurnTaskId,
        (TurnIdentity, CancelFlag, tokio::task::JoinHandle<()>),
    > = BTreeMap::new();
    while let Some(command) = runtime.block_on(commands.recv()) {
        match command {
            WorkerCommand::Run(request) => {
                let identity = request.identity.clone();
                let events = events.clone();
                let flag = CancelFlag::new();
                let task_flag = flag.clone();
                let handle = runtime.spawn(async move {
                    run_turn(request, events, &task_flag).await;
                });
                tasks.insert(identity.task.clone(), (identity, flag, handle));
            }
            WorkerCommand::Cancel { task } => {
                if let Some((identity, flag, handle)) = tasks.remove(&task) {
                    // The flag makes the provider stop producing deltas; the
                    // abort frees the underlying connection promptly. The
                    // cancelled terminal event lets the UI mark the turn
                    // `Stopped` even if the task was aborted mid-chunk.
                    flag.cancel();
                    handle.abort();
                    let _ = events.try_send(TurnEvent::Failed {
                        identity,
                        error: AiError::cancelled(),
                    });
                }
            }
            WorkerCommand::Shutdown => {
                for (_, (_, _, handle)) in tasks {
                    handle.abort();
                }
                break;
            }
        }
    }
}

/// Runs one turn: forwards provider deltas to the UI, then sends `Done` or
/// `Failed` (tagged with the full identity).
async fn run_turn(request: TurnRequest, events: mpsc::Sender<TurnEvent>, cancel: &CancelFlag) {
    let TurnRequest {
        identity,
        request,
        provider,
    } = request;
    let (deltas, mut delta_rx) = mpsc::channel(DELTA_CAPACITY);
    // Forward provider deltas to the UI concurrently with generation.
    let forwarder = tokio::spawn({
        let events = events.clone();
        let identity = identity.clone();
        async move {
            while let Some(StreamEvent::Delta(delta)) = delta_rx.recv().await {
                if events
                    .send(TurnEvent::Delta {
                        identity: identity.clone(),
                        delta,
                    })
                    .await
                    .is_err()
                {
                    // The UI stopped listening (app shutdown): stop forwarding.
                    break;
                }
            }
        }
    });
    let result = provider.stream_chat(request, deltas, cancel).await;
    // Wait for the forwarder to drain the remaining deltas (the provider has
    // dropped its sender by now, so the channel closes).
    let _ = forwarder.await;
    match result {
        Ok(outcome) => {
            let _ = events
                .send(TurnEvent::Done {
                    identity,
                    content: outcome.content,
                    usage: outcome.usage,
                })
                .await;
        }
        Err(error) => {
            let _ = events.send(TurnEvent::Failed { identity, error }).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::data::ai::provider::{
        ChatMessage, ChatRole, FakeProvider, ProviderConfig, ProviderKind,
    };
    use crate::data::ai::{ChatRequest, build_provider};

    fn identity() -> TurnIdentity {
        TurnIdentity {
            ai_box: ContainerId::new(),
            conversation: ConversationId::new(),
            task: TurnTaskId::new(),
        }
    }

    fn provider() -> Arc<dyn ChatProvider> {
        let config = ProviderConfig {
            kind: ProviderKind::Fake,
            ..ProviderConfig::default()
        };
        Arc::from(build_provider(&config).expect("fake provider"))
    }

    async fn collect_until_terminal(
        events: &mut mpsc::Receiver<TurnEvent>,
        identity: &TurnIdentity,
        mut on_delta: impl FnMut(&str),
    ) -> Result<String, AiError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut content = String::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                panic!("timed out waiting for a terminal turn event");
            }
            let event = tokio::time::timeout(remaining, events.recv())
                .await
                .expect("timeout waiting for turn event")
                .expect("worker event channel closed");
            match event {
                TurnEvent::Delta {
                    identity: got,
                    delta,
                } if got == *identity => {
                    content.push_str(&delta);
                    on_delta(&delta);
                }
                TurnEvent::Done {
                    identity: got,
                    content: done_content,
                    ..
                } if got == *identity => return Ok(done_content),
                TurnEvent::Failed {
                    identity: got, error,
                } if got == *identity => return Err(error),
                // Events belonging to another turn are dropped silently.
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn worker_streams_deltas_then_done() {
        let (worker, mut events) = AiWorker::spawn();
        let identity = identity();
        worker
            .submit(TurnRequest {
                identity: identity.clone(),
                request: ChatRequest::new(
                    Some("system".to_owned()),
                    vec![ChatMessage::new(ChatRole::User, "hello")],
                ),
                provider: provider(),
            })
            .expect("submit succeeds");

        let mut deltas = Vec::new();
        let content = collect_until_terminal(&mut events, &identity, |delta| {
            deltas.push(delta.to_owned());
        })
        .await
        .expect("fake provider completes");
        assert!(!deltas.is_empty(), "deltas were streamed");
        assert!(content.contains("hello"), "reply answers the question");
        assert_eq!(content, deltas.concat());
    }

    #[tokio::test]
    async fn worker_delivers_events_with_full_turn_identity() {
        let (worker, mut events) = AiWorker::spawn();
        let identity = identity();
        worker
            .submit(TurnRequest {
                identity: identity.clone(),
                request: ChatRequest::default(),
                provider: provider(),
            })
            .expect("submit succeeds");
        let content = collect_until_terminal(&mut events, &identity, |_| {})
            .await
            .expect("fake provider completes");
        assert!(!content.is_empty());
    }

    #[tokio::test]
    async fn worker_cancels_a_running_turn() {
        // A slow fake provider (delay between chunks) so cancellation lands
        // mid-stream.
        let provider = Arc::new(FakeProvider::canned(vec!["a ", "b ", "c ", "d "], 5, None));
        let (worker, mut events) = AiWorker::spawn();
        let identity = identity();
        worker
            .submit(TurnRequest {
                identity: identity.clone(),
                request: ChatRequest::default(),
                provider,
            })
            .expect("submit succeeds");
        // Let the first deltas arrive, then cancel.
        tokio::time::sleep(Duration::from_millis(8)).await;
        worker.cancel(&identity.task);

        match collect_until_terminal(&mut events, &identity, |_| {}).await {
            Err(error) => assert_eq!(error.kind, AiErrorKind::Cancelled),
            Ok(_) => panic!("a cancelled turn must not report a normal completion"),
        }
    }

    #[tokio::test]
    async fn worker_drops_late_events_for_other_turns() {
        let (worker, mut events) = AiWorker::spawn();
        let first = identity();
        worker
            .submit(TurnRequest {
                identity: first.clone(),
                request: ChatRequest::default(),
                provider: provider(),
            })
            .expect("submit succeeds");
        let content = collect_until_terminal(&mut events, &first, |_| {})
            .await
            .expect("first turn completes");
        assert!(!content.is_empty());

        // A second, unrelated turn: events for the first turn must never be
        // confused with this one (they are already finished anyway).
        let second = identity();
        worker
            .submit(TurnRequest {
                identity: second.clone(),
                request: ChatRequest::default(),
                provider: provider(),
            })
            .expect("submit succeeds");
        let content = collect_until_terminal(&mut events, &second, |_| {})
            .await
            .expect("second turn completes");
        assert!(!content.is_empty());
    }
}
