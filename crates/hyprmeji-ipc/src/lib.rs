// crates/hyprmeji-ipc/src/lib.rs
#![deny(clippy::all)]
//! # hyprmeji-ipc
//!
//! Client IPC Hyprland pour hyprmeji.
//!
//! Ce crate se connecte aux deux sockets exposés par Hyprland (résolus via la
//! variable d'environnement `$HYPRLAND_INSTANCE_SIGNATURE`) :
//! - `.socket.sock`  — requêtes one-shot (ici `j/clients` pour la liste des
//!   fenêtres) ;
//! - `.socket2.sock` — flux d'événements asynchrones au format `event>>data\n`.
//!
//! Il maintient un [`WindowList`] thread-safe ([`Arc`]`<`[`RwLock`]`<…>>`)
//! mis à jour en continu par un thread d'arrière-plan : à intervalle régulier
//! (polling 200 ms) et à chaque événement pertinent (`openwindow`,
//! `closewindow`, `movewindow`).
//!
//! **Invariants du crate :** aucune dépendance Wayland, aucun runtime async.
//! Seuls `std::os::unix::net::UnixStream` et `std::thread` sont utilisés.
//!
//! ## Exemple
//! ```no_run
//! let client = hyprmeji_ipc::IpcClient::new()?;
//! let windows = client.window_list();
//! let _handle = client.start_listener();
//! // Lecture non bloquante depuis la boucle principale :
//! let snapshot = windows.read().clone();
//! println!("{} fenêtres", snapshot.len());
//! # Ok::<(), hyprmeji_ipc::IpcError>(())
//! ```

mod client;
mod error;
mod types;

pub use client::{IpcClient, WindowList};
pub use error::IpcError;
pub use types::WindowInfo;