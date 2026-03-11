use crate::fs_api::invalid_request;
use crate::fs_api::map_io_error;
use crate::fs_api::require_absolute_path;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use codex_app_server_protocol::FsChangedNotification;
use codex_app_server_protocol::FsUnwatchParams;
use codex_app_server_protocol::FsUnwatchResponse;
use codex_app_server_protocol::FsWatchParams;
use codex_app_server_protocol::FsWatchResponse;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ServerNotification;
use notify::Event;
use notify::EventKind;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

const FS_CHANGED_NOTIFICATION_DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Clone)]
pub(crate) struct FsWatchManager {
    outgoing: Arc<OutgoingMessageSender>,
    next_watch_id: Arc<AtomicU64>,
    state: Arc<AsyncMutex<FsWatchState>>,
}

#[derive(Default)]
struct FsWatchState {
    entries: HashMap<WatchPathKey, FsWatchEntry>,
    watch_index: HashMap<WatchKey, WatchPathKey>,
}

struct FsWatchEntry {
    subscriptions: Arc<AsyncMutex<HashMap<WatchKey, FsWatchSubscription>>>,
    cancel: CancellationToken,
    _watcher: RecommendedWatcher,
}

#[derive(Clone)]
struct FsWatchSubscription {
    path: PathBuf,
    filter_path: Option<PathBuf>,
    last_observed_state: Option<ObservedPathState>,
    pending_changed_paths: BTreeSet<PathBuf>,
    notification_tx: mpsc::Sender<()>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct WatchKey {
    connection_id: ConnectionId,
    watch_id: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct WatchPathKey {
    path: PathBuf,
}

struct ResolvedFsWatch {
    path: PathBuf,
    watch_path_key: WatchPathKey,
    filter_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedPathState {
    is_directory: bool,
    is_file: bool,
    len: u64,
    modified_at: Option<SystemTime>,
}

impl FsWatchManager {
    pub(crate) fn new(outgoing: Arc<OutgoingMessageSender>) -> Self {
        Self {
            outgoing,
            next_watch_id: Arc::new(AtomicU64::new(0)),
            state: Arc::new(AsyncMutex::new(FsWatchState::default())),
        }
    }

    pub(crate) async fn watch(
        &self,
        connection_id: ConnectionId,
        params: FsWatchParams,
    ) -> Result<FsWatchResponse, JSONRPCErrorError> {
        require_absolute_path(&params.path, "fs/watch", "path")?;
        let resolved = resolve_fs_watch(params).await?;
        let watch_id = format!(
            "fs-watch-{}",
            self.next_watch_id.fetch_add(1, Ordering::Relaxed)
        );
        let watch_key = WatchKey {
            connection_id,
            watch_id: watch_id.clone(),
        };
        let (notification_tx, notification_rx) = mpsc::channel(1);
        let subscription = FsWatchSubscription {
            path: resolved.path.clone(),
            filter_path: resolved.filter_path.clone(),
            last_observed_state: if resolved.filter_path.is_some() {
                observe_path_state(&resolved.path)
                    .await
                    .map_err(map_io_error)?
            } else {
                None
            },
            pending_changed_paths: BTreeSet::new(),
            notification_tx,
        };

        let mut maybe_watch_task = None;
        let notification_task;
        {
            let mut state = self.state.lock().await;
            if let Some(entry) = state.entries.get(&resolved.watch_path_key) {
                let subscriptions = entry.subscriptions.clone();
                state
                    .watch_index
                    .insert(watch_key.clone(), resolved.watch_path_key.clone());
                subscriptions
                    .lock()
                    .await
                    .insert(watch_key.clone(), subscription);
                notification_task = (subscriptions, notification_rx);
            } else {
                let (raw_tx, raw_rx) = mpsc::unbounded_channel();
                let mut watcher = notify::recommended_watcher(move |res| {
                    let _ = raw_tx.send(res);
                })
                .map_err(map_notify_error)?;
                watcher
                    .watch(&resolved.watch_path_key.path, RecursiveMode::NonRecursive)
                    .map_err(map_notify_error)?;

                let subscriptions = Arc::new(AsyncMutex::new(HashMap::from([(
                    watch_key.clone(),
                    subscription,
                )])));
                let cancel = CancellationToken::new();
                state.entries.insert(
                    resolved.watch_path_key.clone(),
                    FsWatchEntry {
                        subscriptions: subscriptions.clone(),
                        cancel: cancel.clone(),
                        _watcher: watcher,
                    },
                );
                state
                    .watch_index
                    .insert(watch_key.clone(), resolved.watch_path_key.clone());
                maybe_watch_task = Some((
                    resolved.watch_path_key.clone(),
                    subscriptions.clone(),
                    cancel,
                    raw_rx,
                ));
                notification_task = (subscriptions, notification_rx);
            }
        }

        if let Some((watch_path_key, subscriptions, cancel, raw_rx)) = maybe_watch_task {
            self.spawn_watch_task(watch_path_key, subscriptions, cancel, raw_rx);
        }
        self.spawn_notification_task(watch_key, notification_task.0, notification_task.1);

        Ok(FsWatchResponse {
            watch_id,
            path: resolved.path,
        })
    }

    pub(crate) async fn unwatch(
        &self,
        connection_id: ConnectionId,
        params: FsUnwatchParams,
    ) -> Result<FsUnwatchResponse, JSONRPCErrorError> {
        let watch_key = WatchKey {
            connection_id,
            watch_id: params.watch_id,
        };
        let mut state = self.state.lock().await;
        let Some(watch_path_key) = state.watch_index.remove(&watch_key) else {
            return Ok(FsUnwatchResponse {});
        };

        let should_remove_entry = if let Some(subscriptions) = state
            .entries
            .get(&watch_path_key)
            .map(|entry| entry.subscriptions.clone())
        {
            let mut subscriptions = subscriptions.lock().await;
            subscriptions.remove(&watch_key);
            subscriptions.is_empty()
        } else {
            false
        };
        if should_remove_entry && let Some(entry) = state.entries.remove(&watch_path_key) {
            entry.cancel.cancel();
        }
        Ok(FsUnwatchResponse {})
    }

    pub(crate) async fn connection_closed(&self, connection_id: ConnectionId) {
        let mut state = self.state.lock().await;
        let mut empty_keys = Vec::new();
        let mut removed_watch_keys = Vec::new();

        for (watch_key, entry) in &state.entries {
            let mut subscriptions = entry.subscriptions.lock().await;
            let removed_for_entry: Vec<WatchKey> = subscriptions
                .keys()
                .filter(|watch_id| watch_id.connection_id == connection_id)
                .cloned()
                .collect();
            for watch_id in &removed_for_entry {
                subscriptions.remove(watch_id);
            }
            removed_watch_keys.extend(removed_for_entry);
            if subscriptions.is_empty() {
                empty_keys.push(watch_key.clone());
            }
        }

        for watch_key in removed_watch_keys {
            state.watch_index.remove(&watch_key);
        }
        for watch_key in empty_keys {
            if let Some(entry) = state.entries.remove(&watch_key) {
                entry.cancel.cancel();
            }
        }
    }

    fn spawn_watch_task(
        &self,
        watch_path_key: WatchPathKey,
        subscriptions: Arc<AsyncMutex<HashMap<WatchKey, FsWatchSubscription>>>,
        cancel: CancellationToken,
        mut raw_rx: mpsc::UnboundedReceiver<notify::Result<Event>>,
    ) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    raw_event = raw_rx.recv() => {
                        match raw_event {
                            Some(Ok(event)) => {
                                if !should_process_event(&event) {
                                    continue;
                                }

                                {
                                    let mut subscriptions = subscriptions.lock().await;
                                    notifications_for_event(&mut subscriptions, &watch_path_key.path, &event)
                                        .await;
                                };
                            }
                            Some(Err(err)) => {
                                warn!(
                                    "filesystem watch error for {}: {err}",
                                    watch_path_key.path.display()
                                );
                            }
                            None => break,
                        }
                    }
                }
            }
        });
    }

    fn spawn_notification_task(
        &self,
        watch_key: WatchKey,
        subscriptions: Arc<AsyncMutex<HashMap<WatchKey, FsWatchSubscription>>>,
        mut notification_rx: mpsc::Receiver<()>,
    ) {
        let outgoing = self.outgoing.clone();
        tokio::spawn(async move {
            while notification_rx.recv().await.is_some() {
                tokio::time::sleep(FS_CHANGED_NOTIFICATION_DEBOUNCE).await;

                let changed_paths = {
                    let mut subscriptions = subscriptions.lock().await;
                    let Some(subscription) = subscriptions.get_mut(&watch_key) else {
                        return;
                    };
                    while notification_rx.try_recv().is_ok() {}
                    if subscription.pending_changed_paths.is_empty() {
                        continue;
                    }
                    std::mem::take(&mut subscription.pending_changed_paths)
                        .into_iter()
                        .collect::<Vec<_>>()
                };

                for changed_path in changed_paths {
                    outgoing
                        .send_server_notification_to_connections(
                            &[watch_key.connection_id],
                            ServerNotification::FsChanged(FsChangedNotification {
                                watch_id: watch_key.watch_id.clone(),
                                changed_path,
                            }),
                        )
                        .await;
                }
            }
        });
    }
}

async fn notifications_for_event(
    subscriptions: &mut HashMap<WatchKey, FsWatchSubscription>,
    watch_root: &Path,
    event: &Event,
) {
    let event_is_ambiguous_for_file_subscriptions =
        event.paths.is_empty() || event.paths.iter().all(|path| path == watch_root);

    for subscription in subscriptions.values_mut() {
        let mut changed_paths_were_mutated = false;
        if let Some(filter_path) = &subscription.filter_path {
            let is_relevant = if event_is_ambiguous_for_file_subscriptions {
                match observe_path_state(&subscription.path).await {
                    Ok(next_state) => {
                        let changed = next_state != subscription.last_observed_state;
                        subscription.last_observed_state = next_state;
                        changed
                    }
                    Err(err) => {
                        warn!(
                            "failed to inspect watched file state for {}: {err}",
                            subscription.path.display()
                        );
                        false
                    }
                }
            } else {
                let is_relevant = event
                    .paths
                    .iter()
                    .any(|path| path_matches_filter(path, filter_path, watch_root));
                if is_relevant {
                    match observe_path_state(&subscription.path).await {
                        Ok(next_state) => {
                            subscription.last_observed_state = next_state;
                        }
                        Err(err) => {
                            warn!(
                                "failed to refresh watched file state for {}: {err}",
                                subscription.path.display()
                            );
                        }
                    }
                }
                is_relevant
            };
            if is_relevant {
                changed_paths_were_mutated = subscription
                    .pending_changed_paths
                    .insert(subscription.path.clone());
            }
        } else if event.paths.is_empty() {
            changed_paths_were_mutated = subscription
                .pending_changed_paths
                .insert(subscription.path.clone());
        } else {
            let mut seen_paths = HashSet::new();
            for changed_path in &event.paths {
                let changed_path = if changed_path == watch_root {
                    subscription.path.clone()
                } else {
                    changed_path.clone()
                };
                if seen_paths.insert(changed_path.clone())
                    && subscription.pending_changed_paths.insert(changed_path)
                {
                    changed_paths_were_mutated = true;
                }
            }
        }

        if changed_paths_were_mutated {
            let _ = subscription.notification_tx.try_send(());
        }
    }
}

async fn observe_path_state(path: &Path) -> io::Result<Option<ObservedPathState>> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(Some(ObservedPathState {
            is_directory: metadata.is_dir(),
            is_file: metadata.is_file(),
            len: metadata.len(),
            modified_at: metadata.modified().ok(),
        })),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn path_matches_filter(changed_path: &Path, filter_path: &Path, watch_root: &Path) -> bool {
    changed_path == filter_path
        || (changed_path.parent() == Some(watch_root)
            && changed_path.file_name() == filter_path.file_name())
}

async fn resolve_fs_watch(params: FsWatchParams) -> Result<ResolvedFsWatch, JSONRPCErrorError> {
    let requested_path = params.path;
    match tokio::fs::metadata(&requested_path).await {
        Ok(metadata) => {
            if metadata.is_dir() {
                let path = tokio::fs::canonicalize(&requested_path)
                    .await
                    .map_err(map_io_error)?;
                return Ok(ResolvedFsWatch {
                    path: path.clone(),
                    watch_path_key: WatchPathKey { path },
                    filter_path: None,
                });
            }

            let path = tokio::fs::canonicalize(&requested_path)
                .await
                .map_err(map_io_error)?;
            let watch_root = path.parent().ok_or_else(|| {
                invalid_request("fs/watch requires path to include a parent directory")
            })?;
            return Ok(ResolvedFsWatch {
                path: path.clone(),
                watch_path_key: WatchPathKey {
                    path: watch_root.to_path_buf(),
                },
                filter_path: Some(path),
            });
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(map_io_error(err)),
    }

    let file_name = requested_path
        .file_name()
        .ok_or_else(|| invalid_request("fs/watch requires path to include a file name"))?;
    let parent = requested_path
        .parent()
        .ok_or_else(|| invalid_request("fs/watch requires path to include a parent directory"))?;
    let watch_root = tokio::fs::canonicalize(parent)
        .await
        .map_err(map_io_error)?;
    let path = watch_root.join(file_name);
    Ok(ResolvedFsWatch {
        path: path.clone(),
        watch_path_key: WatchPathKey { path: watch_root },
        filter_path: Some(path),
    })
}

fn should_process_event(event: &Event) -> bool {
    match event.kind {
        EventKind::Access(_) => false,
        EventKind::Any
        | EventKind::Create(_)
        | EventKind::Modify(_)
        | EventKind::Remove(_)
        | EventKind::Other => true,
    }
}

fn map_notify_error(err: notify::Error) -> JSONRPCErrorError {
    JSONRPCErrorError {
        code: crate::error_code::INTERNAL_ERROR_CODE,
        message: err.to_string(),
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outgoing_message::OutgoingEnvelope;
    use crate::outgoing_message::OutgoingMessage;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    async fn file_subscription(path: &Path) -> (FsWatchSubscription, mpsc::Receiver<()>) {
        let (notification_tx, notification_rx) = mpsc::channel(1);
        (
            FsWatchSubscription {
                path: path.to_path_buf(),
                filter_path: Some(path.to_path_buf()),
                last_observed_state: observe_path_state(path)
                    .await
                    .expect("should capture file state"),
                pending_changed_paths: BTreeSet::new(),
                notification_tx,
            },
            notification_rx,
        )
    }

    fn expect_fs_changed_notification(
        envelope: OutgoingEnvelope,
    ) -> (ConnectionId, FsChangedNotification) {
        match envelope {
            OutgoingEnvelope::ToConnection {
                connection_id,
                message:
                    OutgoingMessage::AppServerNotification(ServerNotification::FsChanged(notification)),
            } => (connection_id, notification),
            envelope => panic!("expected fs/changed notification, got {envelope:?}"),
        }
    }

    #[tokio::test]
    async fn ambiguous_watch_root_event_notifies_only_the_file_that_changed() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path();
        let head_path = watch_root.join("HEAD");
        let fetch_head_path = watch_root.join("FETCH_HEAD");
        std::fs::write(&head_path, "old-head\n").expect("write HEAD");
        std::fs::write(&fetch_head_path, "old-fetch\n").expect("write FETCH_HEAD");

        let mut subscriptions = HashMap::from([
            (
                WatchKey {
                    connection_id: ConnectionId(1),
                    watch_id: "head".to_string(),
                },
                file_subscription(&head_path).await.0,
            ),
            (
                WatchKey {
                    connection_id: ConnectionId(2),
                    watch_id: "fetch".to_string(),
                },
                file_subscription(&fetch_head_path).await.0,
            ),
        ]);

        std::fs::write(&head_path, "new-head\n").expect("update HEAD");

        notifications_for_event(
            &mut subscriptions,
            watch_root,
            &Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
                .add_path(watch_root.to_path_buf()),
        )
        .await;

        assert_eq!(
            subscriptions
                .get(&WatchKey {
                    connection_id: ConnectionId(1),
                    watch_id: "head".to_string(),
                })
                .expect("head subscription should exist")
                .pending_changed_paths,
            BTreeSet::from([head_path])
        );
    }

    #[tokio::test]
    async fn ambiguous_empty_paths_event_notifies_only_the_file_that_changed() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path();
        let head_path = watch_root.join("HEAD");
        let fetch_head_path = watch_root.join("FETCH_HEAD");
        std::fs::write(&head_path, "old-head\n").expect("write HEAD");
        std::fs::write(&fetch_head_path, "old-fetch\n").expect("write FETCH_HEAD");

        let mut subscriptions = HashMap::from([
            (
                WatchKey {
                    connection_id: ConnectionId(1),
                    watch_id: "head".to_string(),
                },
                file_subscription(&head_path).await.0,
            ),
            (
                WatchKey {
                    connection_id: ConnectionId(2),
                    watch_id: "fetch".to_string(),
                },
                file_subscription(&fetch_head_path).await.0,
            ),
        ]);

        std::fs::write(&fetch_head_path, "new-fetch\n").expect("update FETCH_HEAD");

        notifications_for_event(
            &mut subscriptions,
            watch_root,
            &Event::new(EventKind::Modify(notify::event::ModifyKind::Any)),
        )
        .await;

        assert_eq!(
            subscriptions
                .get(&WatchKey {
                    connection_id: ConnectionId(2),
                    watch_id: "fetch".to_string(),
                })
                .expect("fetch subscription should exist")
                .pending_changed_paths,
            BTreeSet::from([fetch_head_path])
        );
    }

    #[tokio::test]
    async fn blocked_sender_coalesces_same_path_notifications_per_watch_key() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path();
        let head_path = watch_root.join("HEAD");
        std::fs::write(&head_path, "old-head\n").expect("write HEAD");

        let watch_key = WatchKey {
            connection_id: ConnectionId(1),
            watch_id: "head".to_string(),
        };
        let (subscription, notification_rx) = file_subscription(&head_path).await;
        let subscriptions = Arc::new(AsyncMutex::new(HashMap::from([(
            watch_key.clone(),
            subscription,
        )])));
        let (tx, mut rx) = mpsc::channel(1);
        let outgoing = Arc::new(OutgoingMessageSender::new(tx));
        let manager = FsWatchManager::new(outgoing);
        manager.spawn_notification_task(watch_key.clone(), subscriptions.clone(), notification_rx);

        {
            let mut subscriptions_guard = subscriptions.lock().await;
            notifications_for_event(
                &mut subscriptions_guard,
                watch_root,
                &Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(head_path.clone()),
            )
            .await;
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let pending_is_empty = {
                    let subscriptions_guard = subscriptions.lock().await;
                    subscriptions_guard
                        .get(&watch_key)
                        .expect("head subscription should exist")
                        .pending_changed_paths
                        .is_empty()
                };
                if pending_is_empty {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("first batch should be drained into the sender");

        {
            let mut subscriptions_guard = subscriptions.lock().await;
            notifications_for_event(
                &mut subscriptions_guard,
                watch_root,
                &Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(head_path.clone()),
            )
            .await;
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let pending_is_empty = {
                    let subscriptions_guard = subscriptions.lock().await;
                    subscriptions_guard
                        .get(&watch_key)
                        .expect("head subscription should exist")
                        .pending_changed_paths
                        .is_empty()
                };
                if pending_is_empty {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("second batch should be waiting on the blocked sender");

        {
            let mut subscriptions_guard = subscriptions.lock().await;
            notifications_for_event(
                &mut subscriptions_guard,
                watch_root,
                &Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(head_path.clone()),
            )
            .await;
        }
        {
            let mut subscriptions_guard = subscriptions.lock().await;
            notifications_for_event(
                &mut subscriptions_guard,
                watch_root,
                &Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(head_path.clone()),
            )
            .await;
        }

        let (connection_id, notification) = expect_fs_changed_notification(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("first notification should arrive")
                .expect("channel should remain open"),
        );
        assert_eq!(connection_id, ConnectionId(1));
        assert_eq!(
            notification,
            FsChangedNotification {
                watch_id: "head".to_string(),
                changed_path: head_path.clone(),
            }
        );

        let (connection_id, notification) = expect_fs_changed_notification(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("blocked notification should arrive")
                .expect("channel should remain open"),
        );
        assert_eq!(connection_id, ConnectionId(1));
        assert_eq!(
            notification,
            FsChangedNotification {
                watch_id: "head".to_string(),
                changed_path: head_path.clone(),
            }
        );

        let (connection_id, notification) = expect_fs_changed_notification(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("coalesced follow-up notification should arrive")
                .expect("channel should remain open"),
        );
        assert_eq!(connection_id, ConnectionId(1));
        assert_eq!(
            notification,
            FsChangedNotification {
                watch_id: "head".to_string(),
                changed_path: head_path,
            }
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(150), rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn unwatch_is_scoped_to_the_connection_that_created_the_watch() {
        let temp_dir = TempDir::new().expect("temp dir");
        let head_path = temp_dir.path().join("HEAD");
        std::fs::write(&head_path, "ref: refs/heads/main\n").expect("write HEAD");

        let (tx, _rx) = mpsc::channel(1);
        let manager = FsWatchManager::new(Arc::new(OutgoingMessageSender::new(tx)));
        let response = manager
            .watch(
                ConnectionId(1),
                FsWatchParams {
                    path: head_path.clone(),
                },
            )
            .await
            .expect("watch should succeed");

        manager
            .unwatch(
                ConnectionId(2),
                FsUnwatchParams {
                    watch_id: response.watch_id.clone(),
                },
            )
            .await
            .expect("foreign unwatch should be a no-op");

        let watch_path_key = WatchPathKey {
            path: head_path
                .parent()
                .expect("watched file should have parent")
                .canonicalize()
                .expect("canonicalize watch root"),
        };
        let watch_key = WatchKey {
            connection_id: ConnectionId(1),
            watch_id: response.watch_id.clone(),
        };
        let state = manager.state.lock().await;
        let entry = state
            .entries
            .get(&watch_path_key)
            .expect("watch entry should remain");
        assert_eq!(state.watch_index.get(&watch_key), Some(&watch_path_key));
        let subscriptions = entry.subscriptions.clone();
        drop(state);
        assert!(subscriptions.lock().await.contains_key(&watch_key));

        manager
            .unwatch(
                ConnectionId(1),
                FsUnwatchParams {
                    watch_id: response.watch_id,
                },
            )
            .await
            .expect("owner unwatch should succeed");
    }
}
