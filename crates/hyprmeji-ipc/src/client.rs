// crates/hyprmeji-ipc/src/client.rs
//! `IpcClient` : connexion aux sockets Hyprland, polling et écoute d'événements.
//!
//! Le client résout les chemins des sockets à la construction (via
//! `$HYPRLAND_INSTANCE_SIGNATURE`) et expose un [`WindowList`] partagé. Le thread
//! lancé par [`IpcClient::start_listener`] alterne entre :
//! - lecture ligne-à-ligne du flux d'événements `.socket2.sock` ;
//! - re-fetch de la liste complète sur `.socket.sock` lorsqu'un événement
//!   pertinent survient ou que l'intervalle de polling (200 ms) est écoulé.
//!
//! Un read timeout court sur `.socket2.sock` permet de réveiller le thread
//! régulièrement pour honorer le polling sans bloquer indéfiniment en lecture,
//! et sans recourir à un runtime async.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::error::IpcError;
use crate::types::{self, WindowInfo};

/// Liste de fenêtres partagée et thread-safe.
pub type WindowList = Arc<RwLock<Vec<WindowInfo>>>;

/// Intervalle de polling de la liste des fenêtres.
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Timeout de lecture sur le socket d'événements, pour réveiller le thread.
const EVENT_READ_TIMEOUT: Duration = Duration::from_millis(100);
/// Requête envoyée au socket de commande pour obtenir la liste des fenêtres.
const CLIENTS_REQUEST: &[u8] = b"j/clients\n";

/// Client IPC Hyprland.
pub struct IpcClient {
    /// Chemin du socket de requêtes one-shot (`.socket.sock`).
    socket_path: PathBuf,
    /// Chemin du socket d'événements (`.socket2.sock`).
    socket2_path: PathBuf,
    /// Liste de fenêtres partagée, mise à jour par le thread d'écoute.
    windows: WindowList,
}

impl IpcClient {
    /// Construit un client en résolvant les chemins des sockets Hyprland.
    ///
    /// Effectue un premier fetch synchrone de la liste des fenêtres afin que
    /// [`IpcClient::window_list`] soit immédiatement exploitable. Si ce fetch
    /// initial échoue (Hyprland indisponible), la liste démarre vide et sera
    /// remplie par le thread d'écoute ; l'erreur est seulement logguée.
    ///
    /// # Erreurs
    /// Retourne [`IpcError::MissingSignature`] si la variable d'environnement
    /// `HYPRLAND_INSTANCE_SIGNATURE` est absente.
    pub fn new() -> Result<Self, IpcError> {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .map_err(IpcError::MissingSignature)?;

        let base = format!("/tmp/hypr/{sig}");
        let socket_path = PathBuf::from(format!("{base}/.socket.sock"));
        let socket2_path = PathBuf::from(format!("{base}/.socket2.sock"));

        let windows: WindowList = Arc::new(RwLock::new(Vec::new()));

        let client = Self {
            socket_path,
            socket2_path,
            windows,
        };

        // Fetch initial best-effort : on ne fait pas échouer la construction si
        // Hyprland n'est pas encore prêt.
        match fetch_window_list(&client.socket_path) {
            Ok(list) => *client.windows.write() = list,
            Err(e) => log::warn!("fetch initial de la liste des fenêtres échoué : {e}"),
        }

        Ok(client)
    }

    /// Retourne un clone de l'`Arc` vers la liste de fenêtres partagée.
    ///
    /// La boucle principale peut lire ce `RwLock` de manière non bloquante.
    #[must_use]
    pub fn window_list(&self) -> WindowList {
        Arc::clone(&self.windows)
    }

    /// Démarre le thread d'écoute des événements et de polling.
    ///
    /// Le thread tourne indéfiniment : il met à jour la liste partagée sur
    /// événement pertinent et à intervalle de polling régulier. Toute erreur
    /// d'I/O est logguée via `log::warn!` puis le thread retente — il ne
    /// panique jamais.
    #[must_use]
    pub fn start_listener(&self) -> JoinHandle<()> {
        let socket_path = self.socket_path.clone();
        let socket2_path = self.socket2_path.clone();
        let windows = Arc::clone(&self.windows);

        thread::spawn(move || {
            listener_loop(&socket_path, &socket2_path, &windows);
        })
    }
}

/// Boucle d'écoute : se (re)connecte au socket d'événements et traite le flux.
///
/// En cas de déconnexion ou d'échec de connexion, attend brièvement puis
/// retente, sans jamais paniquer.
fn listener_loop(socket_path: &PathBuf, socket2_path: &PathBuf, windows: &WindowList) {
    loop {
        match UnixStream::connect(socket2_path) {
            Ok(stream) => {
                if let Err(e) = stream.set_read_timeout(Some(EVENT_READ_TIMEOUT)) {
                    log::warn!("impossible de régler le read timeout : {e}");
                }
                consume_events(stream, socket_path, windows);
                log::warn!("flux d'événements socket2 interrompu, reconnexion…");
            }
            Err(e) => {
                log::warn!(
                    "connexion au socket d'événements {} échouée : {e}",
                    socket2_path.display()
                );
            }
        }
        // Évite une boucle de reconnexion serrée si Hyprland est absent.
        thread::sleep(POLL_INTERVAL);
    }
}

/// Lit le flux d'événements ligne par ligne et déclenche les re-fetch.
///
/// Le read timeout fait que `read_line` peut renvoyer `WouldBlock`/`TimedOut` :
/// on s'en sert pour vérifier périodiquement l'échéance de polling.
fn consume_events(stream: UnixStream, socket_path: &PathBuf, windows: &WindowList) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let mut last_poll = Instant::now();

    // Premier remplissage à la connexion.
    refresh(socket_path, windows);

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // EOF : le socket a été fermé côté Hyprland.
                return;
            }
            Ok(_) => {
                if event_requires_refresh(&line) {
                    refresh(socket_path, windows);
                    last_poll = Instant::now();
                }
            }
            Err(e) if is_timeout(&e) => {
                // Pas de nouvel événement : on vérifie l'échéance de polling.
            }
            Err(e) => {
                log::warn!("lecture du socket d'événements échouée : {e}");
                return;
            }
        }

        if last_poll.elapsed() >= POLL_INTERVAL {
            refresh(socket_path, windows);
            last_poll = Instant::now();
        }
    }
}

/// Re-fetch la liste des fenêtres et met à jour le `RwLock` partagé.
fn refresh(socket_path: &PathBuf, windows: &WindowList) {
    match fetch_window_list(socket_path) {
        Ok(list) => *windows.write() = list,
        Err(e) => log::warn!("re-fetch de la liste des fenêtres échoué : {e}"),
    }
}

/// Effectue une requête one-shot `j/clients` et désérialise la réponse.
fn fetch_window_list(socket_path: &PathBuf) -> Result<Vec<WindowInfo>, IpcError> {
    let mut stream =
        UnixStream::connect(socket_path).map_err(|e| IpcError::connect(socket_path, e))?;
    stream
        .write_all(CLIENTS_REQUEST)
        .map_err(|e| IpcError::io(socket_path, e))?;
    // Signale qu'on n'écrira plus rien : Hyprland peut renvoyer puis fermer.
    if let Err(e) = stream.shutdown(std::net::Shutdown::Write) {
        log::warn!("shutdown write du socket de commande échoué : {e}");
    }

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| IpcError::io(socket_path, e))?;

    let windows = types::parse_clients(&response)?;
    Ok(windows)
}

/// Indique si une ligne d'événement socket2 doit déclencher un re-fetch.
///
/// Format attendu : `event>>data\n`. Seuls `openwindow`, `closewindow` et
/// `movewindow` nous intéressent ; les autres sont ignorés silencieusement.
fn event_requires_refresh(line: &str) -> bool {
    matches!(parse_event_name(line), Some("openwindow" | "closewindow" | "movewindow"))
}

/// Extrait le nom d'événement d'une ligne socket2 (`event>>data`).
///
/// Retourne `None` si la ligne est vide ou malformée.
fn parse_event_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return None;
    }
    // Le nom est tout ce qui précède le premier `>>`.
    match trimmed.find(">>") {
        Some(idx) => Some(&trimmed[..idx]),
        // Une ligne sans `>>` n'est pas un événement exploitable.
        None => None,
    }
}

/// Vrai si l'erreur d'I/O correspond à un timeout de lecture (read timeout).
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_event_name() {
        assert_eq!(parse_event_name("openwindow>>0x55,1,class,title\n"), Some("openwindow"));
        assert_eq!(parse_event_name("closewindow>>0x55\n"), Some("closewindow"));
        assert_eq!(parse_event_name("movewindow>>0x55,2\n"), Some("movewindow"));
        assert_eq!(parse_event_name("workspace>>3\n"), Some("workspace"));
    }

    #[test]
    fn parses_event_name_without_trailing_newline() {
        assert_eq!(parse_event_name("activewindow>>foo"), Some("activewindow"));
    }

    #[test]
    fn empty_or_malformed_lines_have_no_name() {
        assert_eq!(parse_event_name(""), None);
        assert_eq!(parse_event_name("\n"), None);
        assert_eq!(parse_event_name("no-separator-here\n"), None);
    }

    #[test]
    fn refresh_only_on_relevant_events() {
        assert!(event_requires_refresh("openwindow>>data\n"));
        assert!(event_requires_refresh("closewindow>>data\n"));
        assert!(event_requires_refresh("movewindow>>data\n"));

        // Événements ignorés.
        assert!(!event_requires_refresh("workspace>>3\n"));
        assert!(!event_requires_refresh("activewindow>>x\n"));
        assert!(!event_requires_refresh("openlayer>>foo\n"));
        assert!(!event_requires_refresh("\n"));
        assert!(!event_requires_refresh("garbage\n"));
    }

    #[test]
    fn event_with_empty_data_still_matches() {
        // `event>>` sans data : le nom seul suffit à décider.
        assert!(event_requires_refresh("movewindow>>\n"));
    }

    #[test]
    fn timeout_kinds_are_detected() {
        let wb = std::io::Error::new(std::io::ErrorKind::WouldBlock, "wb");
        let to = std::io::Error::new(std::io::ErrorKind::TimedOut, "to");
        let other = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "bp");
        assert!(is_timeout(&wb));
        assert!(is_timeout(&to));
        assert!(!is_timeout(&other));
    }

    #[test]
    #[ignore = "nécessite un vrai socket Hyprland"]
    fn new_requires_signature() {
        // Ne s'exécute que dans un environnement Hyprland réel.
        let _ = IpcClient::new();
    }
}