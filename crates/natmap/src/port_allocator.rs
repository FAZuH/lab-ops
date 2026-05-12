use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;

use color_eyre::Result;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::info;

pub struct PortAllocator {
    sockets: RwLock<HashMap<String, TcpListener>>,
}

impl Default for PortAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl PortAllocator {
    pub fn new() -> Self {
        Self {
            sockets: RwLock::new(HashMap::new()),
        }
    }

    pub async fn allocate(&self, key: &str, addr: SocketAddr) -> Result<()> {
        let bind_addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), addr.port());
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("Port {} already in use: {}", addr.port(), e))?;
        info!("Port {} reserved for {}", addr.port(), key);
        self.sockets.write().await.insert(key.to_string(), listener);
        Ok(())
    }

    pub async fn deallocate(&self, key: &str) {
        self.sockets.write().await.remove(key);
        info!("Port released for {}", key);
    }

    pub async fn is_allocated(&self, key: &str) -> bool {
        self.sockets.read().await.contains_key(key)
    }

    pub async fn deallocate_all(&self) {
        let count = self.sockets.write().await.len();
        self.sockets.write().await.clear();
        info!("Released all {} port reservations", count);
    }
}
