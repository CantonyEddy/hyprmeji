// crates/hyprmeji/src/main.rs
#![deny(clippy::all)]

//! Point d'entrée du binaire `hyprmeji`.
//!
//! Ce binaire ne contient **aucune** logique métier : il se limite à
//!   1. parser la CLI,
//!   2. initialiser les crates dans le bon ordre,
//!   3. démarrer le thread IPC,
//!   4. installer la gestion des signaux OS,
//!   5. déléguer à la boucle principale (`r#loop`).
//!
//! Toute la logique (états, physique, animation, Wayland, input) vit dans les
//! crates `hyprmeji-core`, `hyprmeji-render` et `hyprmeji-ipc`. L'input souris
//! est géré en interne par `hyprmeji-render` : le binaire ne construit plus de
//! `InputHandler` et se contente d'appeler `renderer.poll_input()`.

mod r#loop;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::Parser;

use hyprmeji_ipc::IpcClient;
use hyprmeji_loader as loader;
use hyprmeji_render::Renderer;

/// Shimeji natif Wayland pour Hyprland.
#[derive(Parser, Debug)]
#[command(name = "hyprmeji", version, about, long_about = None)]
struct Cli {
    /// Chemin vers le répertoire du shimeji (format natif TOML ou Java).
    #[arg(value_name = "CHEMIN-VERS-RÉPERTOIRE-SHIMEJI")]
    shimeji_dir: PathBuf,
}

fn main() -> ExitCode {
    env_logger::init();

    let cli = Cli::parse();

    match run(cli) {
        Ok(()) => {
            log::info!("hyprmeji: arrêt propre");
            ExitCode::SUCCESS
        }
        Err(err) => {
            log::error!("hyprmeji: erreur fatale: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Séquence d'initialisation puis exécution de la boucle.
///
/// Renvoie `Err` pour toute erreur fatale de démarrage (loader, ipc, render) ;
/// l'appelant logge et quitte avec le code 1.
fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // --- 1. Drapeau d'arrêt partagé, armé par les signaux OS ---------------
    let shutdown = Arc::new(AtomicBool::new(false));
    install_signal_handlers(&shutdown)?;

    // --- 2. Chargement du shimeji (filesystem + décodage PNG) --------------
    // Erreur fatale si le répertoire est invalide ou illisible.
    let sprite_sheet = loader::load(cli.shimeji_dir.as_path()).map_err(|e| {
        format!(
            "chargement du shimeji « {} » : {e}",
            cli.shimeji_dir.display()
        )
    })?;
    log::info!("shimeji chargé depuis {}", cli.shimeji_dir.display());

    // --- 3. IPC Hyprland : client + thread d'écoute ------------------------
    let ipc = IpcClient::new().map_err(|e| format!("connexion IPC Hyprland : {e}"))?;
    let window_list = ipc.window_list();
    let _ipc_handle = ipc.start_listener();
    log::info!("thread IPC Hyprland démarré");

    // --- 4. Rendu Wayland (surface overlay layer-shell + input intégré) ----
    // L'input souris est construit et géré en interne par le renderer.
    let renderer = Renderer::new().map_err(|e| format!("initialisation du rendu Wayland : {e}"))?;
    log::info!("surface Wayland initialisée");

    // --- 5. Boucle principale 60fps --------------------------------------
    r#loop::run(r#loop::Context {
        sprite_sheet,
        window_list,
        renderer,
        shutdown,
    });

    Ok(())
}

/// Installe les handlers SIGTERM/SIGINT via `signal-hook`.
fn install_signal_handlers(shutdown: &Arc<AtomicBool>) -> Result<(), Box<dyn std::error::Error>> {
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(sig, Arc::clone(shutdown))
            .map_err(|e| format!("enregistrement du signal {sig} : {e}"))?;
    }
    // Garantit que le drapeau démarre désarmé.
    shutdown.store(false, Ordering::SeqCst);
    Ok(())
}
