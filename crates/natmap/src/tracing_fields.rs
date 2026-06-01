#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use axum::Router;
    use bollard::models::EventMessage;
    use bollard::models::EventActor;
    use crate::daemon::Daemon;
    use crate::daemon::AppState;
    use crate::iptables::IptablesManager;
    use crate::models::DaemonState;
    use lab_ops_lab_lib::port::PortAllocator;
    use tokio::sync::RwLock;
    use tracing_test::traced_test;
    use bollard::Docker;

    fn create_test_daemon(state_path: PathBuf) -> Daemon {
        let iptables = Arc::new(IptablesManager::new());
        let ports = Arc::new(PortAllocator::new());
        let daemon_state = Arc::new(RwLock::new(DaemonState::default()));

        let state = AppState {
            daemon_state,
            iptables,
            docker: None,
            state_path,
            next_id: Arc::new(AtomicU64::new(1)),
            ports,
            socket_group: "root".to_string(),
            socket_path: PathBuf::from("/tmp/natmap.sock"),
        };

        Daemon {
            state,
            app: Router::new(),
        }
    }

    #[tokio::test]
    #[traced_test]
    async fn reload_state_logs_mapping_count() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state_path = temp_dir.path().join("state.json");
        
        let daemon = create_test_daemon(state_path);
        
        // Ensure reload runs and produces logs
        let _ = daemon.reload().await; // ignore iptables flush error
        
        // Assert we logged the mapping count
        assert!(logs_contain("mappings.count="));
    }

    #[tokio::test]
    #[traced_test]
    async fn handle_docker_event_span_has_container_id() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state_path = temp_dir.path().join("state.json");
        
        let daemon = create_test_daemon(state_path);
        // We pass a dummy docker object, or we can just mock the event since the actual docker calls will just return
        let docker = Docker::connect_with_local_defaults().unwrap();
        
        let event = EventMessage {
            action: Some("start".to_string()),
            actor: Some(EventActor {
                id: Some("1234567890".to_string()),
                ..Default::default()
            }),
            typ: Some(bollard::plugin::EventMessageTypeEnum::CONTAINER),
            ..Default::default()
        };

        daemon.handle_docker_event(event, &docker).await;
        
        assert!(logs_contain("container.id=\"1234567890\""));
    }
}
