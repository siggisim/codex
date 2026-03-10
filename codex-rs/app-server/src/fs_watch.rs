use crate::fs_api::invalid_request;
use crate::fs_api::map_io_error;
use crate::fs_api::require_absolute_path;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use codex_app_server_protocol::FsChangedNotification;
use codex_app_server_protocol::FsUnwatchParams;
use codex_app_server_protocol::FsUnwatchResponse;
use codex_app_server_protocol::FsWatchEventType;
use codex_app_server_protocol::FsWatchParams;
use codex_app_server_protocol::FsWatchResponse;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ServerNotification;
use notify::Event;
use notify::EventKind;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use notify::event::ModifyKind;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

#[derive(Clone)]
pub(crate) struct FsWatchManager {
    outgoing: Arc<OutgoingMessageSender>,
    next_watch_id: Arc<AtomicU64>,
    state: Arc<AsyncMutex<FsWatchState>>,
}

#[derive(Default)]
struct FsWatchState {
    entries: HashMap<WatchKey, FsWatchEntry>,
    watch_index: HashMap<String, WatchKey>,
}

struct FsWatchEntry {
    subscriptions: Arc<Mutex<HashMap<String, FsWatchSubscription>>>,
    cancel: CancellationToken,
    _watcher: RecommendedWatcher,
}

#[derive(Clone)]
struct FsWatchSubscription {
    connection_id: ConnectionId,
    path: PathBuf,
    filter_path: Option<PathBuf>,
    last_observed_state: Option<ObservedPathState>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct WatchKey {
    path: PathBuf,
}

struct ResolvedFsWatch {
    path: PathBuf,
    watch_key: WatchKey,
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
        let subscription = FsWatchSubscription {
            connection_id,
            path: resolved.path.clone(),
            filter_path: resolved.filter_path.clone(),
            last_observed_state: if resolved.filter_path.is_some() {
                observe_path_state(&resolved.path).map_err(map_io_error)?
            } else {
                None
            },
        };

        let mut maybe_task = None;
        {
            let mut state = self.state.lock().await;
            if let Some(entry) = state.entries.get(&resolved.watch_key) {
                entry
                    .subscriptions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(watch_id.clone(), subscription);
                state
                    .watch_index
                    .insert(watch_id.clone(), resolved.watch_key.clone());
            } else {
                let (raw_tx, raw_rx) = mpsc::unbounded_channel();
                let mut watcher = notify::recommended_watcher(move |res| {
                    let _ = raw_tx.send(res);
                })
                .map_err(map_notify_error)?;
                watcher
                    .watch(&resolved.watch_key.path, RecursiveMode::NonRecursive)
                    .map_err(map_notify_error)?;

                let subscriptions = Arc::new(Mutex::new(HashMap::from([(
                    watch_id.clone(),
                    subscription,
                )])));
                let cancel = CancellationToken::new();
                state.entries.insert(
                    resolved.watch_key.clone(),
                    FsWatchEntry {
                        subscriptions: subscriptions.clone(),
                        cancel: cancel.clone(),
                        _watcher: watcher,
                    },
                );
                state
                    .watch_index
                    .insert(watch_id.clone(), resolved.watch_key.clone());
                maybe_task = Some((resolved.watch_key.clone(), subscriptions, cancel, raw_rx));
            }
        }

        if let Some((watch_key, subscriptions, cancel, raw_rx)) = maybe_task {
            self.spawn_watch_task(watch_key, subscriptions, cancel, raw_rx);
        }

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
        let watch_id = &params.watch_id;
        let mut state = self.state.lock().await;
        let Some(watch_key) = state.watch_index.get(watch_id).cloned() else {
            return Ok(FsUnwatchResponse {});
        };

        let mut should_remove_entry = false;
        let mut should_remove_watch_index = false;
        let mut has_connection_mismatch = false;
        if let Some(entry) = state.entries.get(&watch_key) {
            {
                let mut subscriptions = entry
                    .subscriptions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match subscriptions.get(watch_id) {
                    Some(subscription) if subscription.connection_id == connection_id => {
                        subscriptions.remove(watch_id);
                        should_remove_watch_index = true;
                        should_remove_entry = subscriptions.is_empty();
                    }
                    Some(_) => {
                        has_connection_mismatch = true;
                    }
                    None => {
                        should_remove_watch_index = true;
                        should_remove_entry = subscriptions.is_empty();
                    }
                }
            };
        } else {
            should_remove_watch_index = true;
        }

        if has_connection_mismatch {
            return Ok(FsUnwatchResponse {});
        }

        if should_remove_watch_index {
            state.watch_index.remove(watch_id);
        }
        if should_remove_entry && let Some(entry) = state.entries.remove(&watch_key) {
            entry.cancel.cancel();
        }
        Ok(FsUnwatchResponse {})
    }

    pub(crate) async fn connection_closed(&self, connection_id: ConnectionId) {
        let mut state = self.state.lock().await;
        let mut empty_keys = Vec::new();
        let mut removed_watch_ids = Vec::new();

        for (watch_key, entry) in &state.entries {
            let mut subscriptions = entry
                .subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            subscriptions.retain(|watch_id, subscription| {
                let keep = subscription.connection_id != connection_id;
                if !keep {
                    removed_watch_ids.push(watch_id.clone());
                }
                keep
            });
            if subscriptions.is_empty() {
                empty_keys.push(watch_key.clone());
            }
        }

        for watch_id in removed_watch_ids {
            state.watch_index.remove(&watch_id);
        }
        for watch_key in empty_keys {
            if let Some(entry) = state.entries.remove(&watch_key) {
                entry.cancel.cancel();
            }
        }
    }

    fn spawn_watch_task(
        &self,
        watch_key: WatchKey,
        subscriptions: Arc<Mutex<HashMap<String, FsWatchSubscription>>>,
        cancel: CancellationToken,
        mut raw_rx: mpsc::UnboundedReceiver<notify::Result<Event>>,
    ) {
        let outgoing = self.outgoing.clone();
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

                                let event_type = map_event_type(event.kind);
                                let notifications = {
                                    let mut subscriptions = subscriptions
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    notifications_for_event(
                                        &mut subscriptions,
                                        &watch_key.path,
                                        &event,
                                        event_type,
                                    )
                                };
                                for (connection_id, notification) in notifications {
                                    outgoing
                                        .send_server_notification_to_connections(
                                            &[connection_id],
                                            ServerNotification::FsChanged(notification),
                                        )
                                        .await;
                                }
                            }
                            Some(Err(err)) => {
                                warn!("filesystem watch error for {}: {err}", watch_key.path.display());
                            }
                            None => break,
                        }
                    }
                }
            }
        });
    }
}

fn notifications_for_event(
    subscriptions: &mut HashMap<String, FsWatchSubscription>,
    watch_root: &Path,
    event: &Event,
    event_type: FsWatchEventType,
) -> Vec<(ConnectionId, FsChangedNotification)> {
    let event_is_ambiguous_for_file_subscriptions =
        event.paths.is_empty() || event.paths.iter().all(|path| path == watch_root);
    let mut notifications = Vec::new();

    for (watch_id, subscription) in subscriptions.iter_mut() {
        if let Some(filter_path) = &subscription.filter_path {
            let is_relevant = if event_is_ambiguous_for_file_subscriptions {
                match observe_path_state(&subscription.path) {
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
                    match observe_path_state(&subscription.path) {
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
                notifications.push((
                    subscription.connection_id,
                    FsChangedNotification {
                        watch_id: watch_id.clone(),
                        changed_path: subscription.path.clone(),
                        event_type,
                    },
                ));
            }
            continue;
        }

        if event.paths.is_empty() {
            notifications.push((
                subscription.connection_id,
                FsChangedNotification {
                    watch_id: watch_id.clone(),
                    changed_path: subscription.path.clone(),
                    event_type,
                },
            ));
            continue;
        }

        let mut seen_paths = HashSet::new();
        for changed_path in &event.paths {
            let changed_path = if changed_path == watch_root {
                subscription.path.clone()
            } else {
                changed_path.clone()
            };
            if seen_paths.insert(changed_path.clone()) {
                notifications.push((
                    subscription.connection_id,
                    FsChangedNotification {
                        watch_id: watch_id.clone(),
                        changed_path,
                        event_type,
                    },
                ));
            }
        }
    }

    notifications
}

fn observe_path_state(path: &Path) -> io::Result<Option<ObservedPathState>> {
    match std::fs::metadata(path) {
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
                    watch_key: WatchKey { path },
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
                watch_key: WatchKey {
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
        watch_key: WatchKey { path: watch_root },
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

fn map_event_type(event_kind: EventKind) -> FsWatchEventType {
    match event_kind {
        EventKind::Create(_) | EventKind::Remove(_) => FsWatchEventType::Rename,
        EventKind::Modify(modify_kind) => match modify_kind {
            ModifyKind::Name(_) => FsWatchEventType::Rename,
            _ => FsWatchEventType::Change,
        },
        EventKind::Any | EventKind::Other => FsWatchEventType::Change,
        EventKind::Access(_) => FsWatchEventType::Change,
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
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn file_subscription(connection_id: u64, path: &Path) -> FsWatchSubscription {
        FsWatchSubscription {
            connection_id: ConnectionId(connection_id),
            path: path.to_path_buf(),
            filter_path: Some(path.to_path_buf()),
            last_observed_state: observe_path_state(path).expect("should capture file state"),
        }
    }

    #[test]
    fn ambiguous_watch_root_event_notifies_only_the_file_that_changed() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path();
        let head_path = watch_root.join("HEAD");
        let fetch_head_path = watch_root.join("FETCH_HEAD");
        std::fs::write(&head_path, "old-head\n").expect("write HEAD");
        std::fs::write(&fetch_head_path, "old-fetch\n").expect("write FETCH_HEAD");

        let mut subscriptions = HashMap::from([
            ("head".to_string(), file_subscription(1, &head_path)),
            ("fetch".to_string(), file_subscription(2, &fetch_head_path)),
        ]);

        std::fs::write(&head_path, "new-head\n").expect("update HEAD");

        let notifications = notifications_for_event(
            &mut subscriptions,
            watch_root,
            &Event::new(EventKind::Modify(ModifyKind::Any)).add_path(watch_root.to_path_buf()),
            FsWatchEventType::Change,
        );

        assert_eq!(
            notifications,
            vec![(
                ConnectionId(1),
                FsChangedNotification {
                    watch_id: "head".to_string(),
                    changed_path: head_path,
                    event_type: FsWatchEventType::Change,
                },
            )]
        );
    }

    #[test]
    fn ambiguous_empty_paths_event_notifies_only_the_file_that_changed() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path();
        let head_path = watch_root.join("HEAD");
        let fetch_head_path = watch_root.join("FETCH_HEAD");
        std::fs::write(&head_path, "old-head\n").expect("write HEAD");
        std::fs::write(&fetch_head_path, "old-fetch\n").expect("write FETCH_HEAD");

        let mut subscriptions = HashMap::from([
            ("head".to_string(), file_subscription(1, &head_path)),
            ("fetch".to_string(), file_subscription(2, &fetch_head_path)),
        ]);

        std::fs::write(&fetch_head_path, "new-fetch\n").expect("update FETCH_HEAD");

        let notifications = notifications_for_event(
            &mut subscriptions,
            watch_root,
            &Event::new(EventKind::Modify(ModifyKind::Any)),
            FsWatchEventType::Change,
        );

        assert_eq!(
            notifications,
            vec![(
                ConnectionId(2),
                FsChangedNotification {
                    watch_id: "fetch".to_string(),
                    changed_path: fetch_head_path,
                    event_type: FsWatchEventType::Change,
                },
            )]
        );
    }
}
