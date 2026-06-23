// crates/hyprmeji/src/loop.rs
#![deny(clippy::all)]

//! Boucle principale 60fps (§9 de l'ARCHITECTURE.md).
//!
//! Orchestration pure : la boucle lit les entrées (input via le renderer,
//! window list, timer idle), les traduit en `Event`, fait avancer la machine
//! d'états (fonction libre `transition`) et la physique (`PhysicsEngine`), puis
//! demande le rendu. Aucune règle métier n'est décidée ici — tout est délégué à
//! `hyprmeji-core`. L'input souris est exposé par `hyprmeji-render` via
//! `Renderer::poll_input`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use hyprmeji_core::{
    transition, AnimationPlayer, Event, PhysicsBody, PhysicsConfig, PhysicsEngine, Rect, Screen,
    State, Vec2,
};
use hyprmeji_input::InputEvent;
use hyprmeji_ipc::WindowInfo;
use hyprmeji_render::Renderer;

/// Durée d'un tick : 16 ms ⇒ ~60 fps.
const TICK: Duration = Duration::from_millis(16);

/// Délai d'inactivité avant de déclencher `IdleTimerFired`, en ms.
const IDLE_TIMEOUT_MS: u32 = 3_000;

/// Tout ce dont la boucle a besoin, assemblé par `main::run`.
pub struct Context {
    pub sprite_sheet: hyprmeji_core::SpriteSheet,
    pub window_list: Arc<RwLock<Vec<WindowInfo>>>,
    pub renderer: Renderer,
    pub shutdown: Arc<AtomicBool>,
}

/// Exécute la boucle jusqu'à réception d'un signal d'arrêt.
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
    let mut last_sprite_w: f32 = 128.0;

    let mut animation = match AnimationPlayer::new(sprite_sheet, "idle") {
        Ok(p) => p,
        Err(e) => {
            log::error!("animation initiale « idle » introuvable : {e}");
            return;
        }
    };

    let dt_ms: u32 = TICK.as_millis() as u32;
    let start = Instant::now();
    let mut tick: u32 = 0;

    log::info!("boucle principale démarrée (tick = {dt_ms} ms)");

    while !shutdown.load(Ordering::SeqCst) {
        // 1./2. Pompage Wayland + lecture de l'input souris (via le renderer).
        if let Err(e) = renderer.pump() {
            log::warn!("pump Wayland échoué : {e}");
        }

        let mut pending: Vec<Event> = Vec::new();

        while let Some(event) = renderer.poll_input() {
            match event {
                InputEvent::DragStart { .. } => {
                    pending.push(Event::DragStart);
                }
                InputEvent::DragMove { .. } => {
                    // La position de drag est pilotée par hyprmeji-input qui va
                    // calculer la vélocité. La physique est suspendue dans ce cas.
                }
                InputEvent::DragEnd { cursor_vel } => {
                    pending.push(Event::DragEnd {
                        velocity: cursor_vel,
                    });
                }
            }
        }

        // 3. WindowList (lecture non-bloquante) → détection de fenêtre proche.
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

        // Tick systématique en fin de file
        pending.push(Event::Tick { dt_ms });

        // 5. Transitions de la machine d'états
        for event in &pending {
            if let Some(next) = transition(&current_state, event) {
                current_state = next;
            }
        }

        // 6. Animation
        animation.sync_to_state(&current_state);
        let frame = match animation.advance(dt_ms) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("advance animation : {e}");
                continue;
            }
        };

        // 7. Physique
        let sprite_dims = (frame.width as f32, frame.height as f32);
        last_sprite_w = sprite_dims.0;

        let (w, h) = renderer.screen_size();
        let screen = Screen {
            width: w as f32,
            height: h as f32,
        };

        let outcome = physics.step(&mut body, &current_state, dt_ms, sprite_dims, screen);

        if outcome.reached_any_edge() {
            if let Some(next) = transition(&current_state, &Event::ReachedEdge) {
                current_state = next;
            }
        }

        let pos = body.pos;
        renderer.set_sprite_pos(pos);

        // 8. Rendu de la frame
        if let Err(e) = renderer.render_frame(&frame, pos) {
            log::warn!("render_frame : {e}");
        }

        // 9. Mise à jour de l'input region
        if let Err(e) = renderer.set_input_region(&frame, pos) {
            log::warn!("set_input_region : {e}");
        }

        // 10. Sommeil
        tick += 1;
        let target = start + TICK * tick;
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        } else {
            log::trace!("tick {tick} en retard de {:?}", now - target);
        }
    }

    log::info!("signal d'arrêt reçu — sortie de la boucle (surface libérée par Drop)");
    drop(renderer);
}

fn window_id(w: &WindowInfo) -> u32 {
    let hex = w.address.trim_start_matches("0x");
    u64::from_str_radix(hex, 16).map(|v| v as u32).unwrap_or(0)
}
