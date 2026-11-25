//! Core types shared by the harness and (later) the UI bridge.

use std::time::Duration;

pub mod artefact;

/// What the cat is doing. Drives the 2D skeletal animation on the UI side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatState {
    Idle,
    Listening,
    Thinking,
    Talking,
    Sleeping,
    /// Left alone for a while: the cat gets a little crazy.
    Bored,
    /// Worn out from being crazy: yawning and drooping, about to nod off.
    Tired,
    /// No network: the cat stops and waits for it to return.
    Waiting,
}

impl CatState {
    /// Name used on the wire to the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            CatState::Idle => "idle",
            CatState::Listening => "listening",
            CatState::Thinking => "thinking",
            CatState::Talking => "talking",
            CatState::Sleeping => "sleeping",
            CatState::Bored => "bored",
            CatState::Tired => "tired",
            CatState::Waiting => "waiting",
        }
    }
}

/// One control governs both sensors, so the user can never be in a state where
/// the mic is muted but the camera is silently live (or vice versa).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Privacy {
    muted: bool,
}

impl Privacy {
    /// Sensors are on by default.
    pub fn new() -> Self {
        Self { muted: false }
    }

    pub fn toggle(&mut self) {
        self.muted = !self.muted;
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    pub fn mic_live(&self) -> bool {
        !self.muted
    }

    pub fn camera_live(&self) -> bool {
        !self.muted
    }

    /// The UI must show an indicator whenever any sensor is live.
    pub fn show_indicator(&self) -> bool {
        self.mic_live() || self.camera_live()
    }
}

impl Default for Privacy {
    fn default() -> Self {
        Self::new()
    }
}

/// How dangerous a tool call is. Only `High` needs spoken confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub fn needs_confirmation(self) -> bool {
        self == Risk::High
    }
}

/// Quiet for this long and the cat gets bored (and a little crazy).
pub const BORED_AFTER: Duration = Duration::from_secs(60);
/// ...and this long before the crazy burns out into yawning.
pub const TIRED_AFTER: Duration = Duration::from_secs(150);
/// ...and this long before she curls up and falls asleep.
pub const SLEEP_AFTER: Duration = Duration::from_secs(180);

/// Everything that decides what the cat is doing right now.
#[derive(Debug, Clone, Copy)]
pub struct Situation {
    pub online: bool,
    pub speaking: bool,
    pub thinking: bool,
    pub hearing_user: bool,
    /// Told to go to sleep: stays asleep until woken, however long it has been.
    pub asleep: bool,
    pub idle_for: Duration,
}

/// Pick the cat's state from what is going on around it.
pub fn next_cat_state(s: Situation) -> CatState {
    if !s.online {
        CatState::Waiting
    } else if s.speaking {
        CatState::Talking
    } else if s.thinking {
        CatState::Thinking
    } else if s.asleep || s.idle_for >= SLEEP_AFTER {
        CatState::Sleeping
    } else if s.hearing_user && s.idle_for < BORED_AFTER {
        // Only a fresh conversation reacts to sound. Once she's bored, tired or asleep, a
        // noisy room doesn't count: she wakes when speech has actually been understood
        // (which resets the idle clock), not when the mic twitches.
        CatState::Listening
    } else if s.idle_for >= TIRED_AFTER {
        CatState::Tired
    } else if s.idle_for >= BORED_AFTER {
        CatState::Bored
    } else {
        CatState::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensors_on_by_default() {
        let p = Privacy::new();
        assert!(p.mic_live() && p.camera_live() && p.show_indicator());
    }

    #[test]
    fn one_mute_controls_both() {
        let mut p = Privacy::new();
        p.toggle();
        assert!(!p.mic_live() && !p.camera_live() && !p.show_indicator());
        p.toggle();
        assert!(p.mic_live() && p.camera_live());
    }

    #[test]
    fn only_high_risk_asks() {
        assert!(!Risk::Low.needs_confirmation());
        assert!(!Risk::Medium.needs_confirmation());
        assert!(Risk::High.needs_confirmation());
    }

    fn sit(idle_secs: u64) -> Situation {
        Situation {
            online: true,
            speaking: false,
            thinking: false,
            hearing_user: false,
            asleep: false,
            idle_for: Duration::from_secs(idle_secs),
        }
    }

    #[test]
    fn offline_waits_regardless_of_everything_else() {
        let s = Situation { online: false, speaking: true, thinking: true, hearing_user: true, ..sit(0) };
        assert_eq!(next_cat_state(s), CatState::Waiting);
    }

    #[test]
    fn calm_then_crazy_then_tired_then_asleep() {
        assert_eq!(next_cat_state(sit(10)), CatState::Idle);
        assert_eq!(next_cat_state(sit(100)), CatState::Bored);
        assert_eq!(next_cat_state(sit(160)), CatState::Tired);
        assert_eq!(next_cat_state(sit(400)), CatState::Sleeping);
        assert_eq!(next_cat_state(sit(100_000)), CatState::Sleeping, "never wakes herself up");
    }

    #[test]
    fn told_to_sleep_sleeps_straight_away_and_through_noise() {
        let s = Situation { asleep: true, hearing_user: true, ..sit(0) };
        assert_eq!(next_cat_state(s), CatState::Sleeping);
    }

    #[test]
    fn room_noise_does_not_wake_bored_tired_or_sleeping_cats() {
        let noisy = |idle| Situation { hearing_user: true, ..sit(idle) };
        assert_eq!(next_cat_state(noisy(5)), CatState::Listening);
        assert_eq!(next_cat_state(noisy(100)), CatState::Bored);
        assert_eq!(next_cat_state(noisy(160)), CatState::Tired);
        assert_eq!(next_cat_state(noisy(500)), CatState::Sleeping);
    }

    #[test]
    fn talking_and_thinking_beat_sleep() {
        let s = Situation { asleep: true, speaking: true, ..sit(0) };
        assert_eq!(next_cat_state(s), CatState::Talking);
        let s = Situation { asleep: true, thinking: true, ..sit(0) };
        assert_eq!(next_cat_state(s), CatState::Thinking);
    }
}
