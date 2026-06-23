// crates/hyprmeji/src/loop.rs
#![deny(clippy::all)]

//! Boucle principale 60fps (§9 de l'ARCHITECTURE.md).
//!
//! Orchestration pure : la boucle lit les entrées (input, window list, timer
//! idle), les traduit en `Event`, fait avancer la machine d'états (fonction
//! libre `transition`) et la physique (`PhysicsEngine`), puis demande le rendu.
//! Aucune règle métier n'est décidée ici — tout est délégué à `hyprmeji-core`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use hyprmeji_core::{
    transition, AnimationPlayer, Event, PhysicsBody, PhysicsConfig, PhysicsEngine, Rect, Screen,
    State, Vec2,
};
use hyprmeji_ipc::WindowInfo;
use hyprmeji_render::Renderer;

/// Durée d'un tick : 16 ms ⇒ ~60 fps.
const TICK: Duration = Duration::from_millis(16);

/// Délai d'inactivité avant de déclencher `IdleTimerFired`, en ms.
///
/// Constante simple en v1 (pas de RNG, pour rester déterministe et sans
/// dépendance supplémentaire). La machine d'états transforme cet événement en
/// départ de marche.
const IDLE_TIMEOUT_MS: u32 = 3_000;

/// Dimensions d'écran utilisées par la physique tant qu'aucune source de
/// géométrie moniteur n'est branchée.
///
/// La physique a besoin des bornes d'écran (sol + bords latéraux). En v1, ni
/// `hyprmeji-render` ni `hyprmeji-ipc` n'exposent les dimensions du moniteur
/// principal au binaire.
// TODO: à exposer dans hyprmeji-render — un accesseur des dimensions du
//       moniteur principal (ex. `Renderer::screen_size(&self) -> (u32, u32)`),
//       pour remplacer ce fallback.
const FALLBACK_SCREEN: Screen = Screen {
    width: 1920.0,
    height: 1080.0,
};

/// Tout ce dont la boucle a besoin, assemblé par `main::run`.
pub struct Context {
    pub sprite_sheet: hyprmeji_core::SpriteSheet,
    pub window_list: Arc<RwLock<Vec<WindowInfo>>>,
    pub renderer: Renderer,
    pub shutdown: Arc<AtomicBool>,
}

/// Exécute la boucle jusqu'à réception d'un signal d'arrêt.
///
/// Ne renvoie rien : les erreurs fatales ont été éliminées au démarrage, et les
/// erreurs ponctuelles (render, animation) sont loggées en `warn!` sans
/// interrompre la boucle. À la sortie, `renderer` est `Drop`é, ce qui détruit la
/// surface Wayland proprement. Voir la LIMITATION sur l'input dans le corps.
pub fn run(ctx: Context) {
    let Context {
        sprite_sheet,
        window_list,
        mut renderer,
        shutdown,
    } = ctx;

    // --- État local de la boucle -----------------------------------------
    let physics = PhysicsEngine::new(PhysicsConfig::default());
    let mut body = PhysicsBody::new(Vec2::new(100.0, 100.0));
    let mut current_state = State::Idle;
    let mut idle_elapsed_ms: u32 = 0;
    // Largeur de sprite du dernier rendu, utilisée par la détection de fenêtre
    // au début du tick (avant que la frame courante ne soit connue). Valeur de
    // départ raisonnable, réajustée à chaque frame.
    let mut last_sprite_w: f32 = 128.0;

    // Le lecteur d'animation démarre sur "idle" : erreur fatale logguée si la
    // feuille ne contient pas cette animation de base.
    let mut animation = match AnimationPlayer::new(sprite_sheet, "idle") {
        Ok(p) => p,
        Err(e) => {
            log::error!("animation initiale « idle » introuvable : {e}");
            return;
        }
    };

    let dt_ms: u32 = TICK.as_millis() as u32;

    // Référence temporelle absolue pour la correction de dérive : l'instant
    // cible de chaque tick est calculé depuis `start`, pas depuis le tick
    // précédent → pas d'accumulation d'erreur de sleep.
    let start = Instant::now();
    let mut tick: u32 = 0;

    log::info!("boucle principale démarrée (tick = {dt_ms} ms)");

    while !shutdown.load(Ordering::SeqCst) {
        // 1./2. Pompage Wayland + lecture de l'input souris.
        //
        // LIMITATION (API publique actuelle) : `hyprmeji-render` n'expose ni de
        // méthode pour drainer sa file d'événements Wayland (`pump` est privé),
        // ni de quoi construire un `InputHandler` (son `InputHandler::new` exige
        // un `QueueHandle<AppState>` où `AppState` est un type privé du crate
        // render). Le binaire ne peut donc pas, avec l'API publique actuelle,
        // pomper la file ni récupérer les événements de drag.
        //
        // Conséquence : le drag souris est inactif tant que `hyprmeji-render`
        // n'expose pas une couture d'intégration (p. ex. `pub fn pump` + une
        // fabrique `attach_input`). C'est un changement dans le crate render,
        // hors du périmètre de ce binaire. Le reste de la boucle (états,
        // physique, animation, rendu, proximité fenêtres) reste pleinement
        // fonctionnel.
        let mut pending: Vec<Event> = Vec::new();

        // 3. WindowList (lecture non-bloquante) → détection de fenêtre proche.
        // La détection elle-même appartient à la physique (`detect_wall`) ; le
        // binaire se contente de traduire `WindowInfo` → `(id, Rect)` et le
        // résultat → `Event::WindowNearby`. Aucune logique métier ici.
        if let Some(windows) = window_list.try_read() {
            let rects: Vec<(u32, Rect)> = windows
                .iter()
                .map(|w| {
                    (
                        window_id(w),
                        Rect::new(w.x as f32, w.y as f32, w.width as f32, w.height as f32),
                    )
                })
                .collect();
            if let Some((id, side)) = physics.detect_wall(&body, last_sprite_w, &rects) {
                // Position du bord accroché, pour information de la machine d'états.
                let (x, y) = rects
                    .iter()
                    .find(|(rid, _)| *rid == id)
                    .map(|(_, r)| (r.x, r.y))
                    .unwrap_or((body.pos.x, body.pos.y));
                pending.push(Event::WindowNearby { id, side, x, y });
            }
        }

        // 4. Timer idle → IdleTimerFired si expiré (uniquement en état Idle).
        if matches!(current_state, State::Idle) {
            idle_elapsed_ms = idle_elapsed_ms.saturating_add(dt_ms);
            if idle_elapsed_ms >= IDLE_TIMEOUT_MS {
                idle_elapsed_ms = 0;
                pending.push(Event::IdleTimerFired);
            }
        } else {
            idle_elapsed_ms = 0;
        }

        // Tick systématique en fin de file (cohérent avec la sémantique §6).
        pending.push(Event::Tick { dt_ms });

        // 5. Transitions de la machine d'états pour chaque événement.
        for event in &pending {
            if let Some(next) = transition(&current_state, event) {
                current_state = next;
            }
        }

        // 6. Animation : se cale sur l'état courant puis avance le timer.
        // `sync_to_state` ne change rien si l'animation est déjà la bonne ou si
        // elle est absente de la feuille (retourne `false`, sans panique).
        // On avance d'abord pour disposer de la frame réelle (et donc de ses
        // dimensions) avant le pas de physique.
        animation.sync_to_state(&current_state);
        let frame = match animation.advance(dt_ms) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("advance animation : {e}");
                continue;
            }
        };

        // 7. Physique → StepOutcome, retraduit en Event puis réinjecté.
        // La taille de sprite provient de la frame réelle ; l'écran d'un
        // fallback tant que la géométrie moniteur n'est pas exposée (cf. const).
        let sprite_dims = (frame.width as f32, frame.height as f32);
        last_sprite_w = sprite_dims.0;
        let outcome = physics.step(
            &mut body,
            &current_state,
            dt_ms,
            sprite_dims,
            FALLBACK_SCREEN,
        );
        // Tout bord atteint (sol, bord d'écran, sommet de fenêtre) se traduit par
        // le même `Event::ReachedEdge` ; la machine d'états décide de la
        // transition appropriée selon l'état courant.
        if outcome.reached_any_edge() {
            if let Some(next) = transition(&current_state, &Event::ReachedEdge) {
                current_state = next;
            }
        }

        let pos = body.pos;

        // 8. Rendu de la frame (erreur non-fatale).
        if let Err(e) = renderer.render_frame(&frame, pos) {
            log::warn!("render_frame : {e}");
        }

        // 9. Mise à jour de l'input region (hit-testing pixel-perfect).
        if let Err(e) = renderer.set_input_region(&frame, pos) {
            log::warn!("set_input_region : {e}");
        }

        // 10. Sommeil jusqu'au prochain tick, avec correction de dérive.
        tick += 1;
        let target = start + TICK * tick;
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        } else {
            // En retard : on ne dort pas, le prochain tick se recale sur
            // l'horloge absolue (pas de rattrapage en rafale).
            log::trace!("tick {tick} en retard de {:?}", now - target);
        }
    }

    log::info!("signal d'arrêt reçu — sortie de la boucle (surface libérée par Drop)");
    // `renderer` est `Drop`é ici : la connexion Wayland se ferme et le
    // compositeur retire l'overlay. Pas d'appel explicite nécessaire.
    drop(renderer);
}

/// Dérive un identifiant numérique stable depuis l'adresse Hyprland.
///
/// L'adresse est de la forme `0x55…` ; on en extrait les 32 bits de poids
/// faible, suffisant pour distinguer les fenêtres en v1.
fn window_id(w: &WindowInfo) -> u32 {
    let hex = w.address.trim_start_matches("0x");
    u64::from_str_radix(hex, 16).map(|v| v as u32).unwrap_or(0)
}