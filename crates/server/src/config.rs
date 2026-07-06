use std::net::SocketAddr;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_listen_address")]
    pub listen_addr: SocketAddr,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: default_listen_address(),
        }
    }
}

fn default_listen_address() -> SocketAddr {
    "[::]:3000".parse().unwrap()
}
