//! One convergence pass over a node's declaration.
//!
//! [`tick`] is the whole of a timer tick: read the declaration from disk,
//! converge the node to it through the same [`converge`] the CLI uses, and log
//! what it found. It carries no state of its own.
//!
//! There is deliberately no loop, no cursor and no "last applied" marker here.
//! A node that already matches its declaration produces a plan with nothing to
//! do, which costs one read of the daemon's mappings and writes nothing. Any
//! marker would be a second source of truth for a fact the daemon's mappings
//! already hold, and the two could disagree — the failure this design exists to
//! rule out. The schedule belongs to systemd: `natmap install --from` writes a
//! timer whose unit runs `natmap apply --from <file> --prune`, which is this
//! function.

use std::path::Path;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use lab_ops_lab_lib::discovery::DiscoveryConfig;

use crate::apply::ConvergeOptions;
use crate::apply::Converged;
use crate::apply::NatmapApply;
use crate::apply::Plan;
use crate::apply::converge;

/// What one convergence pass read and did.
#[derive(Debug, Clone)]
pub struct Tick {
    /// The declaration this tick read, as it was on disk.
    pub declaration: DiscoveryConfig,
    /// The outcome: applied, or refused because a port is held elsewhere.
    pub converged: Converged,
}

impl Tick {
    /// The plan this tick computed.
    pub fn plan(&self) -> &Plan {
        match &self.converged {
            Converged::Applied(plan) | Converged::Refused(plan) => plan,
        }
    }

    /// Whether the node already matched its declaration when the tick ran.
    pub fn was_converged(&self) -> bool {
        self.plan().is_clean()
    }
}

/// Reads the declaration and converges the node to it.
///
/// The declaration is re-read on every call, so an edit is picked up by the next
/// tick with nothing else to invalidate. A declaration that cannot be read is an
/// error and writes nothing: a bad path or a half-written file must never be
/// mistaken for an empty declaration and prune the node.
///
/// Span fields: `declaration.path`.
#[tracing::instrument(skip_all, fields(
    node = tracing::field::Empty,
    declaration.path = %declaration.display()
))]
pub async fn tick(
    apply: &dyn NatmapApply,
    declaration: &Path,
    options: ConvergeOptions,
) -> Result<Tick> {
    let config = DiscoveryConfig::load(declaration)
        .wrap_err_with(|| format!("cannot read the declaration {}", declaration.display()))?;
    tracing::Span::current().record("node", &config.node.name);

    let converged = converge(apply, &config, options).await?;
    let tick = Tick {
        declaration: config,
        converged,
    };
    record(&tick, options.dry_run);
    Ok(tick)
}

/// Logs one tick as a single event: what changed, or that nothing did.
///
/// One canonical line per unit of work: the counts are fields, so a converged
/// tick stays quiet at the default level and a journal query can find every
/// tick that changed something. What changed, port by port, is the plan the
/// caller prints.
fn record(tick: &Tick, dry_run: bool) {
    let plan = tick.plan();
    let node = &tick.declaration.node.name;

    if tick.was_converged() && !matches!(tick.converged, Converged::Refused(_)) {
        tracing::debug!(node = %node, changed = false, "converge tick");
        return;
    }

    tracing::info!(
        node = %node,
        changed = true,
        dry_run,
        created = plan.create.len(),
        updated = plan.update.len(),
        released = plan.remove.len(),
        stale = plan.stale.len(),
        kept = plan.keep.len(),
        covered = plan.covered.len(),
        refused = plan.conflicts.len(),
        "converge tick"
    );
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::future::Future;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use axum::Json;
    use axum::extract::Path as ApiPath;
    use axum::extract::State;
    use tracing_test::traced_test;

    use super::*;
    use crate::api::add_mapping;
    use crate::apply::NatmapApply;
    use crate::client::NatmapClient;
    use crate::client::NatmapError;
    use crate::daemon::AppState;
    use crate::daemon::Daemon;
    use crate::daemon::tests::FakeIptables;
    use crate::daemon::tests::spawn_daemon;
    use crate::daemon::tests::test_daemon_and_state;
    use crate::daemon::tests::test_port;
    use crate::models::DockerAddMapRequest;
    use crate::models::DockerPortMap;
    use crate::models::DockerPortMapRequest;
    use crate::models::PolicyRouteConfig;
    use crate::models::TransportProtocol;
    use crate::models::service_mapping_key;

    /// A client that counts the calls that could write, so "a converged tick
    /// writes nothing" is checked at the request too, not only at the daemon:
    /// a re-post of an unchanged mapping installs nothing, and would otherwise
    /// pass for a tick that never looked at the plan.
    struct CountingApply {
        client: NatmapClient,
        writes: Arc<AtomicUsize>,
    }

    impl NatmapApply for CountingApply {
        fn add_mapping(
            &self,
            id: &str,
            req: DockerAddMapRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DockerPortMap, NatmapError>> + Send + '_>> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            let id = id.to_string();
            Box::pin(async move { self.client.add_mapping(&id, req).await })
        }

        fn list_mappings(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<DockerPortMap>, NatmapError>> + Send + '_>>
        {
            Box::pin(async move { self.client.mappings().await })
        }

        fn remove_mapping(
            &self,
            id: &str,
            port: u16,
        ) -> Pin<Box<dyn Future<Output = Result<(), NatmapError>> + Send + '_>> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            let id = id.to_string();
            Box::pin(async move { self.client.remove_mapping(&id, port).await })
        }

        fn policy_route(
            &self,
            config: PolicyRouteConfig,
            delete: bool,
        ) -> Pin<Box<dyn Future<Output = Result<Option<PolicyRouteConfig>, NatmapError>> + Send + '_>>
        {
            Box::pin(async move { self.client.policy_route(config, delete).await })
        }
    }

    /// A daemon over a fake iptables, the client that talks to it, and the
    /// declaration it converges to.
    struct Harness {
        dir: tempfile::TempDir,
        /// Keeps the socket alive; dropping it unlinks the socket file.
        _socket_dir: tempfile::TempDir,
        daemon: Daemon,
        state: AppState,
        client: CountingApply,
        writes: Arc<AtomicUsize>,
        iptables: Arc<FakeIptables>,
    }

    impl Harness {
        async fn start() -> Harness {
            let dir = tempfile::tempdir().unwrap();
            let iptables = Arc::new(FakeIptables::default());
            let (daemon, state) =
                test_daemon_and_state(dir.path().join("state.json"), iptables.clone());
            let (socket_dir, socket) = spawn_daemon(state.clone()).await;
            let writes = Arc::new(AtomicUsize::new(0));
            Harness {
                dir,
                _socket_dir: socket_dir,
                daemon,
                state,
                client: CountingApply {
                    client: NatmapClient::new(socket),
                    writes: writes.clone(),
                },
                writes,
                iptables,
            }
        }

        /// Writes a declaration for `service` owning `ports`, all published.
        fn declaration(&self, service: &str, ports: &[u16]) -> PathBuf {
            let claims: String = ports
                .iter()
                .map(|port| format!("      - port: {port}\n"))
                .collect();
            let published: String = ports
                .iter()
                .map(|port| format!("{port}, "))
                .collect::<String>();
            let path = self.dir.path().join("discovery.yaml");
            fs::write(
                &path,
                format!(
                    "node:\n  name: test-node\n  tailnet_ip: 100.64.0.17\n\
                     services:\n  {service}:\n    type: docker\n    ports:\n{claims}    publish: [{}]\n",
                    published.trim_end_matches(", ")
                ),
            )
            .unwrap();
            path
        }

        /// How many calls that could write the tick has made so far.
        fn writes(&self) -> usize {
            self.writes.load(Ordering::SeqCst)
        }

        /// One tick against this harness's daemon.
        async fn tick(&self, declaration: &Path) -> Result<Tick> {
            tick(
                &self.client,
                declaration,
                ConvergeOptions {
                    prune: true,
                    dry_run: false,
                },
            )
            .await
        }

        /// What a state-file write would change. `persist` renames a fresh temp
        /// file over the path, so the inode moves even when the bytes match.
        fn state_file(&self) -> StateFile {
            let path = self.dir.path().join("state.json");
            let meta = fs::metadata(&path).unwrap();
            StateFile {
                inode: meta.ino(),
                modified: (meta.mtime(), meta.mtime_nsec()),
                len: meta.len(),
                bytes: fs::read(&path).unwrap(),
            }
        }

        /// A converged mapping, seeded without a tick so it logs nothing.
        async fn seed_declared(&self, port: u16) {
            let mut mapping = DockerPortMap::new(
                1,
                DockerPortMapRequest {
                    host_addr: format!("100.64.0.17:{port}").parse().unwrap(),
                    container_addr: format!("127.0.0.1:{port}").parse().unwrap(),
                    proto: TransportProtocol::Tcp,
                },
                service_mapping_key("fazuh-uptime"),
                "fazuh-uptime".to_string(),
            );
            mapping.owner = Some("fazuh-uptime".to_string());
            let key = service_mapping_key("fazuh-uptime");
            self.state
                .daemon_state
                .write()
                .await
                .mapping
                .insert(key, vec![mapping]);
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct StateFile {
        inode: u64,
        modified: (i64, i64),
        len: u64,
        bytes: Vec<u8>,
    }

    #[tokio::test]
    async fn tick_creates_the_mapping_the_declaration_names() {
        let harness = Harness::start().await;
        let port = test_port();
        let declaration = harness.declaration("fazuh-uptime", &[port]);

        let tick = harness.tick(&declaration).await.unwrap();

        assert_eq!(tick.plan().create.len(), 1);
        assert!(!tick.was_converged());
        assert_eq!(harness.iptables.installed_mappings().len(), 1);
        let lock = harness.state.daemon_state.read().await;
        let stored = &lock.mapping[&service_mapping_key("fazuh-uptime")][0];
        assert_eq!(stored.request.host_addr.port(), port);
        assert_eq!(stored.owner.as_deref(), Some("fazuh-uptime"));
    }

    /// AC2: a converged tick writes nothing — no iptables call, no state file.
    #[tokio::test]
    async fn tick_writes_nothing_when_the_node_matches_its_declaration() {
        let harness = Harness::start().await;
        let port = test_port();
        let declaration = harness.declaration("fazuh-uptime", &[port]);
        harness.tick(&declaration).await.unwrap();
        let installed = harness.iptables.installed_mappings().len();
        let removed = harness.iptables.removed_mappings().len();
        let writes = harness.writes();
        let state_file = harness.state_file();

        let tick = harness.tick(&declaration).await.unwrap();

        assert!(tick.was_converged(), "{:?}", tick.plan());
        assert_eq!(
            harness.writes(),
            writes,
            "a converged tick must send no call that writes"
        );
        assert_eq!(
            harness.iptables.installed_mappings().len(),
            installed,
            "a converged tick must install no rule"
        );
        assert_eq!(
            harness.iptables.removed_mappings().len(),
            removed,
            "a converged tick must remove no rule"
        );
        assert_eq!(
            harness.state_file(),
            state_file,
            "a converged tick must not write the state file"
        );
    }

    /// AC1: an edited declaration is picked up by the next tick.
    #[tokio::test]
    async fn tick_picks_up_an_edited_declaration() {
        let harness = Harness::start().await;
        let first = test_port();
        let second = test_port();
        let declaration = harness.declaration("fazuh-uptime", &[first]);
        harness.tick(&declaration).await.unwrap();

        harness.declaration("fazuh-uptime", &[second]);
        let tick = harness.tick(&declaration).await.unwrap();

        assert_eq!(tick.plan().create.len(), 1);
        assert_eq!(tick.plan().remove.len(), 1);
        assert_eq!(
            harness.iptables.removed_mappings()[0]
                .request
                .host_addr
                .port(),
            first
        );
        let lock = harness.state.daemon_state.read().await;
        let stored = &lock.mapping[&service_mapping_key("fazuh-uptime")];
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].request.host_addr.port(), second);
    }

    /// The tick and a Docker reconnect must agree: a reload replays the declared
    /// mapping, so the next tick is a no-op instead of recreating what the
    /// reload dropped.
    #[tokio::test]
    async fn tick_writes_nothing_after_a_docker_reload() {
        let harness = Harness::start().await;
        let port = test_port();
        let declaration = harness.declaration("fazuh-uptime", &[port]);
        harness.tick(&declaration).await.unwrap();

        harness.daemon.reload().await.unwrap();
        let installed = harness.iptables.installed_mappings().len();
        let writes = harness.writes();
        let state_file = harness.state_file();

        let tick = harness.tick(&declaration).await.unwrap();

        assert!(tick.was_converged(), "{:?}", tick.plan());
        assert_eq!(
            harness.writes(),
            writes,
            "a reloaded tick must not re-apply"
        );
        assert_eq!(harness.iptables.installed_mappings().len(), installed);
        assert!(harness.iptables.removed_mappings().is_empty());
        assert_eq!(harness.state_file(), state_file);
    }

    /// The timer is what makes a lost mapping self-healing rather than a
    /// standing disagreement: whatever a reload or a crash left behind, the
    /// next tick puts the declaration's mapping back.
    #[tokio::test]
    async fn tick_restores_a_mapping_the_daemon_lost() {
        let harness = Harness::start().await;
        let port = test_port();
        let declaration = harness.declaration("fazuh-uptime", &[port]);
        harness.tick(&declaration).await.unwrap();
        let installed = harness.iptables.installed_mappings().len();

        // What a reload leaves between its flush and its replay.
        harness.state.daemon_state.write().await.mapping.clear();
        harness.state.ports.deallocate_all().await;

        let tick = harness.tick(&declaration).await.unwrap();

        assert_eq!(tick.plan().create.len(), 1);
        assert_eq!(harness.iptables.installed_mappings().len(), installed + 1);
        assert!(
            harness
                .state
                .daemon_state
                .read()
                .await
                .mapping
                .contains_key(&service_mapping_key("fazuh-uptime"))
        );
    }

    /// AC3: prune never touches a mapping this node did not create.
    #[tokio::test]
    async fn tick_never_prunes_a_hand_added_mapping() {
        let harness = Harness::start().await;
        let port = test_port();
        let declaration = harness.declaration("fazuh-uptime", &[port]);
        harness.tick(&declaration).await.unwrap();
        let hand_port = test_port();
        add_mapping(
            State(harness.state.clone()),
            ApiPath("hand".to_string()),
            Json(DockerAddMapRequest {
                host_ip: "127.0.0.5".into(),
                host_port: hand_port,
                container_port: hand_port,
                target_ip: Some("127.0.0.1".into()),
                proto: TransportProtocol::Tcp,
                owner: None,
            }),
        )
        .await
        .unwrap();

        let tick = harness.tick(&declaration).await.unwrap();

        assert!(tick.was_converged(), "{:?}", tick.plan());
        assert_eq!(tick.plan().keep.len(), 1, "the hand-added mapping is kept");
        assert!(harness.iptables.removed_mappings().is_empty());
        assert!(
            harness
                .state
                .daemon_state
                .read()
                .await
                .mapping
                .contains_key("hand")
        );
    }

    /// A declaration that cannot be read must fail without touching the node:
    /// a wrong path is not an empty declaration.
    #[tokio::test]
    async fn tick_fails_without_touching_the_node_when_the_declaration_is_missing() {
        let harness = Harness::start().await;
        let port = test_port();
        let declaration = harness.declaration("fazuh-uptime", &[port]);
        harness.tick(&declaration).await.unwrap();
        let state_file = harness.state_file();
        let missing = harness.dir.path().join("absent.yaml");

        let result = harness.tick(&missing).await;

        assert!(result.is_err());
        assert!(harness.iptables.removed_mappings().is_empty());
        assert_eq!(harness.state_file(), state_file);
    }

    #[tokio::test]
    #[traced_test]
    async fn tick_logs_what_it_changed() {
        let harness = Harness::start().await;
        let port = test_port();
        let declaration = harness.declaration("fazuh-uptime", &[port]);

        harness.tick(&declaration).await.unwrap();

        assert!(logs_contain("converge tick"));
        assert!(logs_contain("created=1"));
    }

    /// A tick that changes nothing is not an info event: the timer must not fill
    /// the journal with a line per tick on a converged node.
    #[tokio::test]
    #[traced_test]
    async fn tick_logs_a_converged_tick_as_unchanged() {
        let harness = Harness::start().await;
        let port = test_port();
        let declaration = harness.declaration("fazuh-uptime", &[port]);
        harness.seed_declared(port).await;

        let tick = harness.tick(&declaration).await.unwrap();

        assert!(tick.was_converged(), "{:?}", tick.plan());
        assert!(logs_contain("changed=false"));
        assert!(!logs_contain("changed=true"));
    }
}
