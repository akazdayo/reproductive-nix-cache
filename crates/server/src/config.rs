use std::{net::SocketAddr, path::PathBuf};

#[derive(Debug)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub database_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: default_listen_address(),
            database_path: PathBuf::from("reproductive-nix-cache.sqlite"),
        }
    }
}

fn default_listen_address() -> SocketAddr {
    "[::]:51337".parse().unwrap()
}
