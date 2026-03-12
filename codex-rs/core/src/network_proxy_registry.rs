use crate::config::NetworkProxySpec;
use crate::config::StartedNetworkProxy;
use anyhow::Result;
use codex_network_proxy::NetworkProxyAuditMetadata;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum NetworkProxyScope {
    SessionDefault,
    Skill { path_to_skills_md: PathBuf },
}

pub(crate) struct NetworkProxyRegistry {
    spec: Option<NetworkProxySpec>,
    managed_network_requirements_enabled: bool,
    audit_metadata: NetworkProxyAuditMetadata,
    proxies: Mutex<HashMap<NetworkProxyScope, Arc<StartedNetworkProxy>>>,
}

impl NetworkProxyRegistry {
    pub(crate) fn new(
        spec: Option<NetworkProxySpec>,
        managed_network_requirements_enabled: bool,
        audit_metadata: NetworkProxyAuditMetadata,
        default_proxy: Option<StartedNetworkProxy>,
    ) -> Self {
        let mut proxies = HashMap::new();
        if let Some(default_proxy) = default_proxy {
            proxies.insert(NetworkProxyScope::SessionDefault, Arc::new(default_proxy));
        }

        Self {
            spec,
            managed_network_requirements_enabled,
            audit_metadata,
            proxies: Mutex::new(proxies),
        }
    }

    pub(crate) async fn get(&self, scope: &NetworkProxyScope) -> Option<Arc<StartedNetworkProxy>> {
        self.proxies.lock().await.get(scope).cloned()
    }

    pub(crate) async fn get_or_start<F, Fut>(
        &self,
        scope: NetworkProxyScope,
        start: F,
    ) -> Result<Option<Arc<StartedNetworkProxy>>>
    where
        F: FnOnce(NetworkProxySpec, bool, NetworkProxyAuditMetadata) -> Fut,
        Fut: Future<Output = std::io::Result<StartedNetworkProxy>>,
    {
        let mut proxies = self.proxies.lock().await;
        if let Some(existing) = proxies.get(&scope).cloned() {
            return Ok(Some(existing));
        }

        let Some(spec) = self.spec.clone() else {
            return Ok(None);
        };

        let started = Arc::new(
            start(
                spec,
                self.managed_network_requirements_enabled,
                self.audit_metadata.clone(),
            )
            .await?,
        );
        proxies.insert(scope, Arc::clone(&started));
        Ok(Some(started))
    }
}
