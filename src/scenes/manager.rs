//! Scene state machine (`Unknown` vs `Active(scene_id)`).
//!
//! Per Section 5.1 & 12 of spec.md:
//! - On startup and immediately after "Restore All" or crash recovery, the scene state is `Unknown`.
//! - While `Unknown`, no scene button is shown as selected (though `last_active_scene_id` may be
//!   shown with a subtle indicator).
//! - Clicking any scene button when `Unknown` is never treated as a no-op; it aligns the target
//!   monitor's windows to that scene in a single click (FR-19, AT-21).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneState {
    /// Initial state at startup or after recovery/restore-all.
    Unknown,
    /// A scene has been actively switched to and confirmed.
    Active(String),
}

impl Default for SceneState {
    fn default() -> Self {
        SceneState::Unknown
    }
}

#[derive(Debug, Clone, Default)]
pub struct SceneStateManager {
    state: SceneState,
    last_known_scene_id: Option<String>,
}

impl SceneStateManager {
    pub fn new(last_known_scene_id: Option<String>) -> Self {
        Self {
            state: SceneState::Unknown,
            last_known_scene_id,
        }
    }

    pub fn state(&self) -> &SceneState {
        &self.state
    }

    pub fn active_scene_id(&self) -> Option<&str> {
        match &self.state {
            SceneState::Active(id) => Some(id.as_str()),
            SceneState::Unknown => None,
        }
    }

    /// Subtle hint of the previous session's scene when `state == Unknown` (Section 2.1 & 12).
    pub fn last_known_scene_id(&self) -> Option<&str> {
        self.last_known_scene_id.as_deref()
    }

    pub fn is_already_active(&self, scene_id: &str) -> bool {
        match &self.state {
            SceneState::Active(current) => current == scene_id,
            SceneState::Unknown => false,
        }
    }

    pub fn set_active(&mut self, scene_id: impl Into<String>) {
        let id = scene_id.into();
        self.last_known_scene_id = Some(id.clone());
        self.state = SceneState::Active(id);
    }

    pub fn reset_to_unknown(&mut self) {
        self.state = SceneState::Unknown;
    }
}
