# hyprmeji — Architecture complète

> Shimeji natif Wayland pour Hyprland, écrit en Rust.
> Version cible : v1.0 — Document de référence pour les instances de développement.

---

## Table des matières

1. [Vue d'ensemble](#1-vue-densemble)
2. [Structure du monorepo](#2-structure-du-monorepo)
3. [Crates et responsabilités](#3-crates-et-responsabilités)
4. [Protocoles Wayland utilisés](#4-protocoles-wayland-utilisés)
5. [Formats de données](#5-formats-de-données)
6. [Machine d'états](#6-machine-détats)
7. [Physique](#7-physique)
8. [IPC Hyprland](#8-ipc-hyprland)
9. [Boucle principale](#9-boucle-principale)
10. [Interfaces entre crates](#10-interfaces-entre-crates)
11. [Dépendances Rust](#11-dépendances-rust)
12. [Périmètre v1 / hors-scope](#12-périmètre-v1--hors-scope)
13. [Conventions de code](#13-conventions-de-code)

---

## 1. Vue d'ensemble

hyprmeji est un démon qui affiche un ou plusieurs personnages animés ("shimejis") sur le bureau Hyprland. Chaque shimeji est rendu sur une surface Wayland transparente de type overlay (via `wlr-layer-shell`), se déplace selon une machine d'états, obéit à une physique 2D simple, et peut s'accrocher aux bords des fenêtres en lisant leur géométrie via l'IPC Hyprland.

### Flux global

```
[config TOML + sprites PNG]
        │
        ▼
  hyprmeji-loader   ←──────────────────────────────┐
  (parse + valide)                                  │
        │                                           │
        ▼                                           │
  hyprmeji-core                            hyprmeji-ipc
  ┌─────────────────────────────┐          (socket Hyprland)
  │  StateMachine               │◄────────── WindowList
  │  PhysicsEngine              │            (positions fenêtres)
  │  AnimationPlayer            │
  └─────────────┬───────────────┘
                │
                ▼
  hyprmeji-render
  (wlr-layer-shell + cairo/pixman)
                │
                ▼
         [surface Wayland — overlay layer]
```

---

## 2. Structure du monorepo

```
hyprmeji/
├── Cargo.toml                  # workspace root
├── Cargo.lock
├── README.md
├── ARCHITECTURE.md             # ce document
│
├── crates/
│   ├── hyprmeji/               # binaire principal (point d'entrée)
│   ├── hyprmeji-core/          # logique métier (states, physics, anim)
│   ├── hyprmeji-render/        # surface Wayland + rendu sprites
│   ├── hyprmeji-ipc/           # client IPC Hyprland
│   ├── hyprmeji-loader/        # chargement config + sprites
│   └── hyprmeji-input/         # gestion souris (drag, pointer Wayland)
│
├── assets/
│   └── default-shimeji/        # shimeji de test fourni avec le projet
│       ├── manifest.toml
│       └── img/
│
├── schemas/
│   ├── manifest.schema.json    # schéma JSON du format natif
│   └── actions.schema.json
│
└── tests/
    ├── integration/
    └── fixtures/
```

### Cargo.toml workspace root

```toml
[workspace]
members = [
    "crates/hyprmeji",
    "crates/hyprmeji-core",
    "crates/hyprmeji-render",
    "crates/hyprmeji-ipc",
    "crates/hyprmeji-loader",
    "crates/hyprmeji-input",
]
resolver = "2"
```

---

## 3. Crates et responsabilités

### `hyprmeji` (binaire)

Point d'entrée unique. Responsabilités :
- Parser les arguments CLI (`clap`)
- Initialiser les autres crates
- Démarrer la boucle d'événements (`calloop`)
- Gérer les signaux OS (SIGTERM, SIGINT)

**Ne contient aucune logique métier.** Tout délègue aux autres crates.

---

### `hyprmeji-core`

Crate pure (pas de dépendances Wayland). Contient :

- `StateMachine` — machine d'états du shimeji (voir §6)
- `PhysicsEngine` — gravité, vélocité, collision (voir §7)
- `AnimationPlayer` — lecture des frames selon l'état courant
- `types.rs` — types partagés (`Vec2`, `Rect`, `AnimationFrame`, `State`…)

**Règle stricte : zéro I/O, zéro Wayland, zéro filesystem.** Testable en isolation complète.

---

### `hyprmeji-render`

Responsabilités :
- Créer et gérer la surface `wlr-layer-shell` (overlay layer, input passthrough)
- Allouer les buffers Wayland (`wl_shm` ou dmabuf)
- Rasteriser les sprites via `cairo-rs` ou copie directe de pixels PNG pré-décodés
- Exposer une API `fn render_frame(frame: &AnimationFrame, pos: Vec2)`

Dépend de : `hyprmeji-core` (types), `smithay-client-toolkit`

---

### `hyprmeji-ipc`

Responsabilités :
- Se connecter au socket IPC Hyprland (`$HYPRLAND_INSTANCE_SIGNATURE`)
- Requêter la liste des fenêtres : `hyprctl clients -j`
- S'abonner aux événements : `openwindow`, `closewindow`, `movewindow`
- Exposer un `WindowList` thread-safe mis à jour en continu

Format de sortie : `Vec<WindowInfo>` avec `{ title, class, x, y, width, height, workspace }`

Dépend de : `serde_json`, `tokio` (ou `std::os::unix::net` si on reste sync)

---

### `hyprmeji-loader`

Responsabilités :
- Détecter le format du répertoire passé en argument (natif TOML vs shimeji Java)
- Parser `manifest.toml` (format natif) ou `actions.xml` + dossier `img/` (format Java)
- Décoder tous les PNG en buffers RGBA pré-chargés en mémoire
- Retourner une `SpriteSheet` prête à l'emploi

Dépend de : `serde`, `toml`, `quick-xml`, `image` (décodage PNG)

---

### `hyprmeji-input`

Responsabilités :
- S'abonner au protocole `wl_pointer` sur la surface layer-shell
- Détecter un clic + drag sur le shimeji
- Émettre des événements `InputEvent::DragStart { pos }`, `InputEvent::DragMove { delta }`, `InputEvent::DragEnd`

Dépend de : `hyprmeji-core` (types), `smithay-client-toolkit`

---

## 4. Protocoles Wayland utilisés

| Protocole | Usage | Crate |
|-----------|-------|-------|
| `wl_compositor` | créer les surfaces | render |
| `wl_shm` | allouer les buffers pixel | render |
| `zwlr_layer_shell_v1` | surface overlay transparente | render |
| `zwlr_layer_surface_v1` | configurer anchor, size, interactivity | render |
| `wl_pointer` | événements souris pour le drag | input |
| `xdg_output` | dimensions et position des moniteurs | render |

### Configuration de la layer surface

```
layer      : OVERLAY
anchor     : aucun (position absolue libre)
size       : 128×128 (ou dimensions du sprite)
margin     : calculé dynamiquement selon pos
keyboard_interactivity : NONE
input_region : vide (passthrough total — la souris traverse sauf sur le sprite)
```

L'input region sera mise à jour à chaque frame pour correspondre exactement aux pixels non-transparents du sprite courant (pixel-perfect hit testing).

---

## 5. Formats de données

### Format natif — `manifest.toml`

```toml
[shimeji]
name    = "Mon Shimeji"
version = "1.0"
author  = "auteur"

[sprites]
sheet   = "img/sheet.png"   # sprite sheet unique (optionnel)
width   = 128               # largeur d'une frame
height  = 128               # hauteur d'une frame

# Ou bien fichiers individuels :
# [sprites.files]
# idle_0 = "img/idle_0.png"
# idle_1 = "img/idle_1.png"

[[animations]]
name   = "idle"
frames = [
  { file = "img/idle_0.png", duration_ms = 500 },
  { file = "img/idle_1.png", duration_ms = 500 },
]

[[animations]]
name   = "walk_right"
frames = [
  { file = "img/walk_0.png", duration_ms = 100 },
  { file = "img/walk_1.png", duration_ms = 100 },
  { file = "img/walk_2.png", duration_ms = 100 },
]
flip_x = false  # true pour walk_left (réutilise walk_right miroir)

[[animations]]
name   = "fall"
frames = [{ file = "img/fall.png", duration_ms = 50 }]

[[animations]]
name   = "climb"
frames = [
  { file = "img/climb_0.png", duration_ms = 150 },
  { file = "img/climb_1.png", duration_ms = 150 },
]

[[animations]]
name   = "drag"
frames = [{ file = "img/drag.png", duration_ms = 100 }]

[[animations]]
name   = "land"
frames = [
  { file = "img/land_0.png", duration_ms = 200 },
  { file = "img/land_1.png", duration_ms = 200 },
]
```

### Format Java shimeji (import)

Structure attendue :
```
shimeji-dir/
├── actions.xml       # définit les comportements
├── behaviors.xml     # définit les transitions
└── img/
    ├── idle1.png
    ├── idle2.png
    ├── walk1.png
    └── ...
```

Le loader lit `actions.xml` pour extraire les noms de fichiers et durées, et les mappe vers les états internes de hyprmeji-core. Les comportements non supportés en v1 sont ignorés avec un warning.

---

## 6. Machine d'états

### États v1

```
         ┌──────────────────────────────────────────────┐
         │                  IDLE                        │
         │  (immobile, animation idle en boucle)        │
         └──┬──────────────────┬───────────────────┬───┘
            │ timer aléatoire  │ bord d'écran       │ drag souris
            ▼                  ▼                    ▼
         WALK              CLIMB_WALL             DRAGGED
         (marche vers       (grimpe le bord        (suit le curseur,
          un bord)           d'une fenêtre)         physique off)
            │                  │                    │
            │ bord atteint      │ sommet atteint     │ relâché
            ▼                  ▼                    ▼
         CLIMB_WALL         WALK / IDLE            FALL
                                                    │
                                              sol atteint
                                                    ▼
                                                  LAND
                                                    │
                                              anim terminée
                                                    ▼
                                                  IDLE
```

### Définition Rust

```rust
// dans hyprmeji-core/src/state.rs

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Idle,
    Walk { direction: Direction },
    ClimbWall { window_id: u32, side: WallSide },
    Fall { velocity: Vec2 },
    Land,
    Dragged,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Direction { Left, Right }

#[derive(Debug, Clone, PartialEq)]
pub enum WallSide { Left, Right }
```

### Transitions

Les transitions sont déclenchées par des `Event` :

```rust
pub enum Event {
    Tick { dt_ms: u32 },          // chaque frame
    ReachedEdge,                   // bord d'écran ou fenêtre atteint
    WindowNearby { id: u32, side: WallSide, x: f32, y: f32 },
    WindowGone { id: u32 },
    DragStart,
    DragEnd { velocity: Vec2 },    // vélocité au moment du relâcher
    LandingAnimDone,
    IdleTimerFired,
}
```

La `StateMachine` est une fonction pure `fn transition(state: &State, event: &Event) -> Option<State>`. Si `None`, l'état ne change pas.

---

## 7. Physique

### Modèle

Physique 2D minimaliste, mise à jour à chaque `Tick`.

```rust
pub struct PhysicsBody {
    pub pos: Vec2,      // position en pixels (coin haut-gauche du sprite)
    pub vel: Vec2,      // vélocité en px/s
    pub on_ground: bool,
    pub on_wall: bool,
}
```

### Constantes (ajustables dans config)

```toml
[physics]
gravity        = 1800.0   # px/s²
max_fall_speed = 1200.0   # px/s (terminal velocity)
walk_speed     = 80.0     # px/s
climb_speed    = 60.0     # px/s
```

### Règles de collision

**Sol (bas de l'écran) :**
- `pos.y + sprite_height >= screen_height` → `vel.y = 0`, `on_ground = true`, état → `Land`

**Bords latéraux :**
- `pos.x <= 0` ou `pos.x + sprite_width >= screen_width` → rebond ou transition vers `ClimbWall`

**Bords de fenêtre :**
- Si un `WindowInfo` est à `±4px` du shimeji et que l'état est `Walk` → transition vers `ClimbWall`
- En `ClimbWall`, `pos.x` est verrouillé au bord de la fenêtre, seul `pos.y` évolue
- Si `pos.y <= window.y` (sommet atteint) → transition vers `Walk` ou `Idle` sur le dessus

**En état `Dragged` :**
- La physique est suspendue
- `pos` suit directement la position du curseur moins un offset (point de saisie)
- Au `DragEnd`, la `vel` est copiée depuis la vélocité moyenne du curseur sur les 5 dernières frames

---

## 8. IPC Hyprland

### Connexion

```rust
// dans hyprmeji-ipc/src/lib.rs

let socket_path = format!(
    "/tmp/hypr/{}/.socket.sock",
    std::env::var("HYPRLAND_INSTANCE_SIGNATURE")?
);
```

### Requête one-shot (liste des fenêtres)

```
echo "j/clients" | socat - UNIX-CONNECT:/tmp/hypr/$SIG/.socket.sock
```

Retourne un JSON array de fenêtres. Polling toutes les 200ms en v1 (suffisant — pas besoin d'events temps réel pour le positionnement).

### Événements asynchrones (socket2)

```
/tmp/hypr/$SIG/.socket2.sock
```

Format : `event>>data\n`

Événements écoutés en v1 :

| Événement | Utilité |
|-----------|---------|
| `openwindow` | ajouter à WindowList |
| `closewindow` | retirer de WindowList, interrompre ClimbWall si nécessaire |
| `movewindow` | mettre à jour géométrie |
| `workspace` | ignorer en v1 |

### Structure WindowInfo

```rust
pub struct WindowInfo {
    pub address: String,
    pub title:   String,
    pub class:   String,
    pub x:       i32,
    pub y:       i32,
    pub width:   u32,
    pub height:  u32,
}
```

---

## 9. Boucle principale

La boucle tourne à 60fps (tick = 16ms). Elle est gérée par `calloop` dans le crate `hyprmeji`.

```
┌─── Boucle 60fps ──────────────────────────────────────────────────────┐
│                                                                        │
│  1. Flush événements Wayland (wl_display::flush)                      │
│  2. Lire événements input (wl_pointer via hyprmeji-input)             │
│     → Si drag : émettre Event::DragMove / DragEnd                    │
│  3. Lire WindowList depuis hyprmeji-ipc (RwLock, non-bloquant)       │
│     → Détecter fenêtres proches → émettre Event::WindowNearby        │
│  4. Avancer timer idle → émettre Event::IdleTimerFired si expiré     │
│  5. StateMachine::transition(current_state, event) → new_state       │
│  6. PhysicsEngine::step(body, state, dt=16ms, windows)               │
│     → Met à jour pos, vel, on_ground                                  │
│  7. AnimationPlayer::advance(dt) → AnimationFrame courante           │
│  8. hyprmeji-render::render_frame(frame, pos)                        │
│     → Mise à jour surface Wayland + commit                            │
│  9. Dormir jusqu'au prochain tick                                      │
│                                                                        │
└────────────────────────────────────────────────────────────────────────┘
```

L'IPC Hyprland tourne dans un thread séparé et écrit dans un `Arc<RwLock<WindowList>>` partagé avec la boucle principale.

---

## 10. Interfaces entre crates

### Types partagés (`hyprmeji-core/src/types.rs`)

```rust
#[derive(Debug, Clone, Copy)]
pub struct Vec2 { pub x: f32, pub y: f32 }

#[derive(Debug, Clone, Copy)]
pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }

pub struct AnimationFrame {
    pub pixels: Arc<[u8]>,   // RGBA, row-major
    pub width:  u32,
    pub height: u32,
    pub flip_x: bool,
}

pub struct SpriteSheet {
    pub animations: HashMap<String, Vec<AnimationFrame>>,
}
```

### API hyprmeji-render

```rust
pub struct Renderer { /* opaque */ }

impl Renderer {
    pub fn new(conn: &WaylandConn, output: &WlOutput) -> Result<Self>;
    pub fn render_frame(&mut self, frame: &AnimationFrame, pos: Vec2) -> Result<()>;
    pub fn set_input_region(&mut self, frame: &AnimationFrame, pos: Vec2) -> Result<()>;
}
```

### API hyprmeji-ipc

```rust
pub struct IpcClient { /* opaque */ }

impl IpcClient {
    pub fn new() -> Result<Self>;
    pub fn window_list(&self) -> Arc<RwLock<Vec<WindowInfo>>>;
    pub fn start_listener(&self) -> JoinHandle<()>;
}
```

### API hyprmeji-input

```rust
pub enum InputEvent {
    DragStart { grab_offset: Vec2 },
    DragMove  { cursor_pos: Vec2 },
    DragEnd   { cursor_vel: Vec2 },
}

pub struct InputHandler { /* opaque */ }

impl InputHandler {
    pub fn poll(&mut self) -> Option<InputEvent>;
}
```

---

## 11. Dépendances Rust

| Crate externe | Version indicative | Usage |
|---------------|--------------------|-------|
| `smithay-client-toolkit` | 0.18 | abstraction Wayland (wl_shm, layer-shell, pointer) |
| `wayland-protocols-wlr` | 0.3 | protocoles wlroots (layer-shell) |
| `cairo-rs` | 0.18 | rendu sprites + compositing alpha |
| `image` | 0.24 | décodage PNG |
| `serde` + `serde_json` | 1 | IPC JSON + config |
| `toml` | 0.8 | config manifest.toml |
| `quick-xml` | 0.31 | import actions.xml shimeji Java |
| `calloop` | 0.12 | boucle d'événements |
| `clap` | 4 | CLI |
| `log` + `env_logger` | — | logging |
| `thiserror` | 1 | gestion d'erreurs |
| `parking_lot` | 0.12 | RwLock performant pour WindowList |

Toutes les dépendances Wayland sont gérées dans `hyprmeji-render` et `hyprmeji-input`. Les autres crates n'ont pas de dépendances système.

---

## 12. Périmètre v1 / hors-scope

### Dans le scope v1

- Surface Wayland overlay transparente (1 shimeji)
- Animations : idle, walk, fall, land, climb, drag
- Physique : gravité, sol, bords d'écran, bords de fenêtres
- Accrochage aux fenêtres Hyprland (lecture IPC)
- Drag souris (protocole `wl_pointer`)
- Import format shimeji Java (actions.xml + img/)
- Format natif manifest.toml
- Config physique ajustable (toml)
- CLI : `hyprmeji <chemin-shimeji>`

### Hors scope v1 (prévu v2+)

- Multi-shimeji (plusieurs instances simultanées)
- Menu clic droit
- Interaction entre shimejis
- Sons
- Spawning (un shimeji qui en crée un autre)
- Tray icon / daemon mode
- Support multi-moniteur avancé (v1 : moniteur principal uniquement)

---

## 13. Conventions de code

- **Édition Rust** : 2021
- **Formatting** : `rustfmt` avec config par défaut
- **Linting** : `clippy` sans warnings tolérés (`#![deny(clippy::all)]` dans chaque crate)
- **Erreurs** : `thiserror` pour les types d'erreur publics, `anyhow` interdit dans les libs
- **Pas de `unwrap()`** dans le code de production — `expect("message contextuel")` toléré uniquement dans les tests
- **Tests** : chaque module de `hyprmeji-core` doit avoir des tests unitaires. `hyprmeji-render` et `hyprmeji-ipc` ont des tests d'intégration séparés dans `tests/integration/`
- **Commits** : format conventionnel — `feat(core): add climb wall state`, `fix(render): correct alpha blending`
- **Branches** : `main` (stable), `dev` (integration), `feat/<crate>/<feature>` pour le développement

---

*Document généré lors de la session de conception initiale. À mettre à jour à chaque décision architecturale majeure.*
