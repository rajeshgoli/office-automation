use serde::{Deserialize, Serialize};

use crate::config::{RoomModeConfig, ThresholdsConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OccupancyState {
    Present,
    Away,
}

impl OccupancyState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Away => "away",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StateConfig {
    pub motion_timeout_seconds: f64,
    pub departure_verification_seconds: f64,
    pub door_open_threshold_minutes: f64,
    pub door_open_away_timeout_minutes: f64,
    pub co2_critical_ppm: i64,
    pub co2_refresh_target_ppm: i64,
    pub contact_sensors_enabled: bool,
}

impl StateConfig {
    pub fn from_thresholds(thresholds: &ThresholdsConfig) -> Self {
        Self::from_thresholds_and_room_mode(thresholds, &RoomModeConfig::default())
    }

    pub fn from_thresholds_and_room_mode(
        thresholds: &ThresholdsConfig,
        room_mode: &RoomModeConfig,
    ) -> Self {
        Self {
            motion_timeout_seconds: thresholds.motion_timeout_seconds as f64,
            departure_verification_seconds: thresholds.departure_verification_seconds as f64,
            door_open_threshold_minutes: thresholds.door_open_threshold_minutes as f64,
            door_open_away_timeout_minutes: thresholds.door_open_away_timeout_minutes as f64,
            co2_critical_ppm: thresholds.co2_critical_ppm,
            co2_refresh_target_ppm: thresholds.co2_refresh_target_ppm,
            contact_sensors_enabled: room_mode.contact_sensors_enabled,
        }
    }
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            motion_timeout_seconds: 60.0,
            departure_verification_seconds: 120.0,
            door_open_threshold_minutes: 5.0,
            door_open_away_timeout_minutes: 5.0,
            co2_critical_ppm: 2000,
            co2_refresh_target_ppm: 500,
            contact_sensors_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SensorState {
    pub mac_last_active: f64,
    pub external_monitor: bool,
    pub motion_detected: bool,
    pub motion_last_seen: f64,
    pub door_open: bool,
    pub door_last_changed: f64,
    #[serde(default)]
    pub door_opened_at: f64,
    #[serde(default)]
    pub door_closed_at: f64,
    pub window_open: bool,
    pub co2_ppm: i64,
    pub last_updated: f64,
}

impl Default for SensorState {
    fn default() -> Self {
        Self {
            mac_last_active: 0.0,
            external_monitor: false,
            motion_detected: false,
            motion_last_seen: 0.0,
            door_open: false,
            door_last_changed: 0.0,
            door_opened_at: 0.0,
            door_closed_at: 0.0,
            window_open: false,
            co2_ppm: 400,
            last_updated: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateTransition {
    pub old_state: OccupancyState,
    pub new_state: OccupancyState,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateStatus {
    pub state: String,
    pub is_present: bool,
    pub presence_signal_active: bool,
    pub safety_interlock: bool,
    pub erv_should_run: bool,
    pub verifying_departure: bool,
    pub in_door_open_mode: bool,
    pub sensors: StateStatusSensors,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct StateStatusSensors {
    pub mac_last_active: f64,
    pub external_monitor: bool,
    pub motion_detected: bool,
    pub door_open: bool,
    pub door_last_changed: f64,
    pub door_opened_at: f64,
    pub door_closed_at: f64,
    pub window_open: bool,
    pub co2_ppm: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateMachine {
    pub config: StateConfig,
    pub state: OccupancyState,
    pub sensors: SensorState,
    last_door_state: Option<bool>,
    departure_verification_deadline: Option<f64>,
    door_open_away_deadline: Option<f64>,
    suppress_next_door_close_departure: bool,
    last_activity_time: f64,
    last_manual_away_at: Option<f64>,
}

impl StateMachine {
    pub fn new(config: StateConfig, now: f64) -> Self {
        Self {
            config,
            state: OccupancyState::Away,
            sensors: SensorState {
                last_updated: now,
                ..SensorState::default()
            },
            last_door_state: None,
            departure_verification_deadline: None,
            door_open_away_deadline: None,
            suppress_next_door_close_departure: false,
            last_activity_time: now,
            last_manual_away_at: None,
        }
    }

    pub fn from_thresholds(thresholds: &ThresholdsConfig, now: f64) -> Self {
        Self::new(StateConfig::from_thresholds(thresholds), now)
    }

    pub fn from_thresholds_and_room_mode(
        thresholds: &ThresholdsConfig,
        room_mode: &RoomModeConfig,
        now: f64,
    ) -> Self {
        Self::new(
            StateConfig::from_thresholds_and_room_mode(thresholds, room_mode),
            now,
        )
    }

    pub fn in_door_open_mode_at(&self, now: f64) -> bool {
        if !self.config.contact_sensors_enabled {
            return false;
        }
        self.sensors.door_open
            && (now - self.sensors.door_last_changed) / 60.0
                >= self.config.door_open_threshold_minutes
    }

    pub fn presence_signal_active_at(&self, now: f64) -> bool {
        if !self.config.contact_sensors_enabled {
            let motion_recent = self.signal_recent_at(self.sensors.motion_last_seen, now);
            let freshness_anchor = self.last_manual_away_at.unwrap_or(0.0);
            let mac_recent = self.sensors.external_monitor
                && self.sensors.mac_last_active > freshness_anchor
                && self.signal_recent_at(self.sensors.mac_last_active, now);
            return mac_recent || motion_recent;
        }

        if self.in_door_open_mode_at(now) {
            let motion_age = now - self.sensors.motion_last_seen;
            let motion_recent =
                self.sensors.motion_detected || motion_age < self.config.motion_timeout_seconds;
            let mac_recent = self.sensors.external_monitor && self.sensors.mac_last_active > 0.0;
            return mac_recent || motion_recent;
        }

        let mac_presence = self.sensors.external_monitor
            && self.sensors.mac_last_active > self.sensors.door_last_changed;
        let motion_age = now - self.sensors.motion_last_seen;
        let motion_recent =
            self.sensors.motion_detected || motion_age < self.config.motion_timeout_seconds;
        let motion_inside = motion_recent
            && !self.sensors.door_open
            && self.sensors.motion_last_seen > self.sensors.door_last_changed;

        mac_presence || motion_inside
    }

    fn signal_recent_at(&self, timestamp: f64, now: f64) -> bool {
        timestamp > 0.0 && now - timestamp < self.config.motion_timeout_seconds
    }

    pub fn safety_interlock_active(&self) -> bool {
        if !self.config.contact_sensors_enabled {
            return false;
        }
        self.sensors.window_open || self.sensors.door_open
    }

    pub fn erv_should_run(&self) -> bool {
        if self.safety_interlock_active() {
            return false;
        }

        match self.state {
            OccupancyState::Present => self.sensors.co2_ppm > self.config.co2_critical_ppm,
            OccupancyState::Away => self.sensors.co2_ppm > self.config.co2_refresh_target_ppm,
        }
    }

    pub fn verifying_departure(&self) -> bool {
        self.departure_verification_deadline.is_some()
    }

    pub fn advance_timers(&mut self, now: f64) -> Option<StateTransition> {
        if !self.config.contact_sensors_enabled {
            self.departure_verification_deadline = None;
            self.door_open_away_deadline = None;
            self.suppress_next_door_close_departure = false;
            return None;
        }

        if self
            .departure_verification_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.departure_verification_deadline = None;
            if self.state == OccupancyState::Present {
                let transition = self.transition_to(OccupancyState::Away);
                self.sensors.motion_last_seen = 0.0;
                self.sensors.motion_detected = false;
                if transition.is_some() {
                    return transition;
                }
            }
        }

        if self
            .door_open_away_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.door_open_away_deadline = None;
            if self.state == OccupancyState::Present && self.in_door_open_mode_at(now) {
                return self.transition_to(OccupancyState::Away);
            }
        }

        None
    }

    pub fn evaluate_at(&mut self, now: f64) -> Option<StateTransition> {
        if let Some(transition) = self.advance_timers(now) {
            return Some(transition);
        }

        let transition = if self.in_door_open_mode_at(now) {
            if self.state == OccupancyState::Away && self.presence_signal_active_at(now) {
                let transition = self.transition_to(OccupancyState::Present);
                self.start_door_open_away_timer(now);
                transition
            } else {
                if self.state == OccupancyState::Present && self.presence_signal_active_at(now) {
                    self.start_door_open_away_timer(now);
                }
                None
            }
        } else {
            let presence_signal_active = self.presence_signal_active_at(now);
            if self.state == OccupancyState::Away && presence_signal_active {
                self.transition_to(OccupancyState::Present)
            } else if !self.config.contact_sensors_enabled
                && self.state == OccupancyState::Present
                && !presence_signal_active
            {
                self.sensors.motion_detected = false;
                self.transition_to(OccupancyState::Away)
            } else {
                None
            }
        };

        self.last_door_state = Some(self.sensors.door_open);
        transition
    }

    pub fn update_mac_occupancy(
        &mut self,
        last_active_timestamp: f64,
        external_monitor: bool,
        now: f64,
    ) -> Option<StateTransition> {
        self.sensors.external_monitor = external_monitor;
        self.sensors.mac_last_active = last_active_timestamp;
        self.sensors.last_updated = now;

        if external_monitor
            && last_active_timestamp > self.sensors.door_last_changed
            && self.verifying_departure()
        {
            self.cancel_departure_verification();
        }

        let timer_transition = self.advance_timers(now);
        self.evaluate_at(now).or(timer_transition)
    }

    pub fn update_motion(&mut self, detected: bool, now: f64) -> Option<StateTransition> {
        self.sensors.motion_detected = detected;
        if detected {
            self.sensors.motion_last_seen = now;
            self.last_activity_time = now;
            if self.verifying_departure() {
                self.cancel_departure_verification();
            }
        }
        self.sensors.last_updated = now;

        let timer_transition = self.advance_timers(now);
        self.evaluate_at(now).or(timer_transition)
    }

    pub fn update_door(&mut self, is_open: bool, now: f64) -> Option<StateTransition> {
        if !self.config.contact_sensors_enabled {
            self.sensors.last_updated = now;
            return self.advance_timers(now);
        }

        let timer_transition = self.advance_timers(now);
        let was_open = self.last_door_state;
        let previous_open = was_open.unwrap_or(self.sensors.door_open);

        self.sensors.door_open = is_open;
        self.sensors.door_last_changed = now;
        if previous_open != is_open {
            if is_open {
                self.sensors.door_opened_at = now;
            } else {
                self.sensors.door_closed_at = now;
            }
        }
        self.sensors.last_updated = now;

        if !is_open && self.suppress_next_door_close_departure {
            self.cancel_door_open_away_timer();
            self.suppress_next_door_close_departure = false;
        } else if was_open == Some(true) && !is_open {
            self.cancel_door_open_away_timer();
            if self.presence_signal_active_at(now) {
                self.cancel_departure_verification();
            } else {
                self.start_departure_verification(now);
            }
        }

        timer_transition.or_else(|| self.evaluate_at(now))
    }

    pub fn restore_door_state(&mut self, is_open: bool, changed_at: f64, now: f64) {
        if !self.config.contact_sensors_enabled {
            self.sensors.last_updated = now;
            return;
        }

        let changed_at = changed_at.min(now).max(0.0);
        self.sensors.door_open = is_open;
        self.sensors.door_last_changed = changed_at;
        if is_open {
            self.sensors.door_opened_at = changed_at;
        } else {
            self.sensors.door_closed_at = changed_at;
        }
        self.sensors.last_updated = now;
        self.last_door_state = Some(is_open);
    }

    pub fn update_window(&mut self, is_open: bool, now: f64) -> Option<StateTransition> {
        if !self.config.contact_sensors_enabled {
            self.sensors.last_updated = now;
            return self.advance_timers(now);
        }

        let timer_transition = self.advance_timers(now);

        self.sensors.window_open = is_open;
        self.sensors.last_updated = now;
        self.evaluate_at(now).or(timer_transition)
    }

    pub fn set_manual_presence(&mut self, present: bool, now: f64) -> Option<StateTransition> {
        self.advance_timers(now);
        let old_state = self.state;

        if present {
            self.sensors.motion_detected = true;
            self.sensors.motion_last_seen = now;
            self.last_activity_time = now;
            if self.verifying_departure() {
                self.cancel_departure_verification();
            }
            if self.config.contact_sensors_enabled && self.sensors.door_open {
                self.suppress_next_door_close_departure = true;
            }
            self.sensors.last_updated = now;
            self.state = OccupancyState::Present;
            self.last_door_state = Some(self.sensors.door_open);
            if self.config.contact_sensors_enabled && self.sensors.door_open {
                self.start_door_open_away_timer(now);
            }
            return (old_state != self.state).then_some(StateTransition {
                old_state,
                new_state: self.state,
            });
        }

        self.cancel_departure_verification();
        self.cancel_door_open_away_timer();
        self.suppress_next_door_close_departure = false;
        self.sensors.motion_detected = false;
        self.sensors.motion_last_seen = 0.0;
        self.sensors.door_last_changed = now;
        self.sensors.last_updated = now;
        self.state = OccupancyState::Away;
        self.last_door_state = Some(self.sensors.door_open);
        self.last_manual_away_at = Some(now);

        (old_state != self.state).then_some(StateTransition {
            old_state,
            new_state: self.state,
        })
    }

    pub fn update_co2(&mut self, ppm: i64, now: f64) {
        self.sensors.co2_ppm = ppm;
        self.sensors.last_updated = now;
    }

    pub fn status_at(&self, now: f64) -> StateStatus {
        StateStatus {
            state: self.state.as_str().to_string(),
            is_present: self.state == OccupancyState::Present,
            presence_signal_active: self.presence_signal_active_at(now),
            safety_interlock: self.safety_interlock_active(),
            erv_should_run: self.erv_should_run(),
            verifying_departure: self.verifying_departure(),
            in_door_open_mode: self.in_door_open_mode_at(now),
            sensors: StateStatusSensors {
                mac_last_active: self.sensors.mac_last_active,
                external_monitor: self.sensors.external_monitor,
                motion_detected: self.sensors.motion_detected,
                door_open: self.sensors.door_open,
                door_last_changed: self.sensors.door_last_changed,
                door_opened_at: self.sensors.door_opened_at,
                door_closed_at: self.sensors.door_closed_at,
                window_open: self.sensors.window_open,
                co2_ppm: self.sensors.co2_ppm,
            },
        }
    }

    fn transition_to(&mut self, new_state: OccupancyState) -> Option<StateTransition> {
        let old_state = self.state;
        if old_state == new_state {
            return None;
        }
        self.state = new_state;
        Some(StateTransition {
            old_state,
            new_state,
        })
    }

    fn start_departure_verification(&mut self, now: f64) {
        if self.config.contact_sensors_enabled && self.state == OccupancyState::Present {
            self.departure_verification_deadline =
                Some(now + self.config.departure_verification_seconds);
        }
    }

    fn cancel_departure_verification(&mut self) {
        self.departure_verification_deadline = None;
    }

    fn start_door_open_away_timer(&mut self, now: f64) {
        if self.config.contact_sensors_enabled && self.state == OccupancyState::Present {
            self.door_open_away_deadline =
                Some(now + self.config.door_open_away_timeout_minutes * 60.0);
        }
    }

    fn cancel_door_open_away_timer(&mut self) {
        self.door_open_away_deadline = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_departure_config() -> StateConfig {
        StateConfig {
            departure_verification_seconds: 10.0,
            ..StateConfig::default()
        }
    }

    fn contact_disabled_config() -> StateConfig {
        StateConfig {
            contact_sensors_enabled: false,
            ..StateConfig::default()
        }
    }

    #[test]
    fn mac_activity_after_door_event_transitions_to_present() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.sensors.door_last_changed = 1_010.0;

        assert_eq!(machine.update_mac_occupancy(1_005.0, true, 1_020.0), None);
        assert_eq!(machine.state, OccupancyState::Away);

        let transition = machine.update_mac_occupancy(1_021.0, true, 1_021.0);
        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Away,
                new_state: OccupancyState::Present,
            })
        );
    }

    #[test]
    fn motion_only_counts_after_door_event_and_when_door_closed() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.sensors.door_last_changed = 1_050.0;
        machine.sensors.motion_last_seen = 1_040.0;

        assert_eq!(machine.evaluate_at(1_060.0), None);
        assert_eq!(machine.state, OccupancyState::Away);

        assert!(machine.update_motion(true, 1_061.0).is_some());
        assert_eq!(machine.state, OccupancyState::Present);

        machine.state = OccupancyState::Away;
        machine.sensors.door_open = true;
        assert_eq!(machine.update_motion(true, 1_070.0), None);
        assert_eq!(machine.state, OccupancyState::Away);
    }

    #[test]
    fn active_motion_before_door_close_does_not_count_as_inside_presence() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);

        machine.update_door(true, 1_010.0);
        assert_eq!(machine.update_motion(true, 1_012.0), None);

        let transition = machine.update_door(false, 1_020.0);

        assert_eq!(transition, None);
        assert_eq!(machine.state, OccupancyState::Away);
        assert!(!machine.verifying_departure());
    }

    #[test]
    fn motion_after_door_close_counts_as_inside_presence() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);

        machine.update_door(true, 1_010.0);
        machine.update_motion(true, 1_012.0);
        machine.update_door(false, 1_020.0);

        let transition = machine.update_motion(true, 1_021.0);

        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Away,
                new_state: OccupancyState::Present,
            })
        );
        assert_eq!(machine.state, OccupancyState::Present);
    }

    #[test]
    fn departure_verification_expires_to_away_and_resets_motion() {
        let mut machine = StateMachine::new(fast_departure_config(), 1_000.0);
        machine.set_manual_presence(true, 1_001.0);
        machine.update_door(true, 1_010.0);
        machine.update_motion(false, 1_019.0);
        machine.update_door(false, 1_020.0);

        assert!(machine.verifying_departure());
        assert_eq!(machine.advance_timers(1_029.0), None);
        assert_eq!(machine.state, OccupancyState::Present);

        let transition = machine.advance_timers(1_030.0);
        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Present,
                new_state: OccupancyState::Away,
            })
        );
        assert!(!machine.sensors.motion_detected);
        assert_eq!(machine.sensors.motion_last_seen, 0.0);
        assert!(!machine.verifying_departure());
    }

    #[test]
    fn mac_activity_cancels_departure_verification() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.set_manual_presence(true, 1_001.0);
        machine.update_door(true, 1_010.0);
        machine.update_motion(false, 1_019.0);
        machine.update_door(false, 1_020.0);

        assert!(machine.verifying_departure());
        machine.update_mac_occupancy(1_021.0, true, 1_022.0);

        assert!(!machine.verifying_departure());
        assert_eq!(machine.advance_timers(1_040.0), None);
        assert_eq!(machine.state, OccupancyState::Present);
    }

    #[test]
    fn fresh_mac_update_after_departure_deadline_cancels_before_away() {
        let mut machine = StateMachine::new(fast_departure_config(), 1_000.0);
        machine.set_manual_presence(true, 1_001.0);
        machine.update_door(true, 1_010.0);
        machine.update_motion(false, 1_019.0);
        machine.update_door(false, 1_020.0);

        assert!(machine.verifying_departure());
        let transition = machine.update_mac_occupancy(1_031.0, true, 1_031.0);

        assert_eq!(transition, None);
        assert_eq!(machine.state, OccupancyState::Present);
        assert!(!machine.verifying_departure());
    }

    #[test]
    fn mac_update_without_external_monitor_does_not_cancel_departure_verification() {
        let mut machine = StateMachine::new(fast_departure_config(), 1_000.0);
        machine.set_manual_presence(true, 1_001.0);
        machine.update_door(true, 1_010.0);
        machine.update_motion(false, 1_019.0);
        machine.update_door(false, 1_020.0);

        assert!(machine.verifying_departure());
        let transition = machine.update_mac_occupancy(1_031.0, false, 1_031.0);

        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Present,
                new_state: OccupancyState::Away,
            })
        );
        assert_eq!(machine.state, OccupancyState::Away);
        assert!(!machine.verifying_departure());
    }

    #[test]
    fn door_close_with_only_stale_active_motion_starts_departure_verification() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.set_manual_presence(true, 1_001.0);
        machine.update_door(true, 1_010.0);
        machine.update_motion(true, 1_012.0);

        let transition = machine.update_door(false, 1_020.0);

        assert_eq!(transition, None);
        assert_eq!(machine.state, OccupancyState::Present);
        assert!(machine.verifying_departure());
    }

    #[test]
    fn motion_after_door_close_cancels_departure_verification() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.set_manual_presence(true, 1_001.0);
        machine.update_door(true, 1_010.0);
        machine.update_motion(true, 1_012.0);
        machine.update_door(false, 1_020.0);

        assert!(machine.verifying_departure());
        assert_eq!(machine.update_motion(true, 1_021.0), None);
        assert!(!machine.verifying_departure());
        assert_eq!(machine.advance_timers(1_040.0), None);
        assert_eq!(machine.state, OccupancyState::Present);
    }

    #[test]
    fn fresh_motion_after_departure_deadline_cancels_before_away() {
        let mut machine = StateMachine::new(fast_departure_config(), 1_000.0);
        machine.set_manual_presence(true, 1_001.0);
        machine.update_door(true, 1_010.0);
        machine.update_motion(false, 1_019.0);
        machine.update_door(false, 1_020.0);

        assert!(machine.verifying_departure());
        let transition = machine.update_motion(true, 1_031.0);

        assert_eq!(transition, None);
        assert_eq!(machine.state, OccupancyState::Present);
        assert!(!machine.verifying_departure());
    }

    #[test]
    fn door_open_mode_uses_activity_and_timer() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.update_door(true, 1_000.0);

        assert!(machine.in_door_open_mode_at(1_301.0));
        let transition = machine.update_motion(true, 1_301.0);
        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Away,
                new_state: OccupancyState::Present,
            })
        );

        let transition = machine.advance_timers(1_601.0);
        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Present,
                new_state: OccupancyState::Away,
            })
        );
    }

    #[test]
    fn manual_present_while_door_open_suppresses_next_door_close_departure() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.update_door(true, 1_000.0);
        machine.set_manual_presence(true, 1_001.0);

        machine.update_door(false, 1_002.0);

        assert_eq!(machine.state, OccupancyState::Present);
        assert!(!machine.verifying_departure());
    }

    #[test]
    fn manual_away_resets_stale_activity_boundary() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.state = OccupancyState::Present;
        machine.sensors.external_monitor = true;
        machine.sensors.mac_last_active = 1_100.0;
        machine.sensors.door_last_changed = 1_040.0;
        machine.sensors.door_closed_at = 1_030.0;
        machine.sensors.motion_detected = true;
        machine.sensors.motion_last_seen = 1_100.0;

        machine.set_manual_presence(false, 1_101.0);

        assert_eq!(machine.state, OccupancyState::Away);
        assert!(!machine.sensors.motion_detected);
        assert_eq!(machine.sensors.motion_last_seen, 0.0);
        assert_eq!(machine.sensors.door_last_changed, 1_101.0);
        assert_eq!(machine.sensors.door_closed_at, 1_030.0);
        assert!(!machine.presence_signal_active_at(1_102.0));
    }

    #[test]
    fn duplicate_door_reports_do_not_reset_transition_timestamps() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.update_door(true, 1_010.0);
        machine.update_door(true, 1_020.0);

        assert!(machine.sensors.door_open);
        assert_eq!(machine.sensors.door_last_changed, 1_020.0);
        assert_eq!(machine.sensors.door_opened_at, 1_010.0);

        machine.update_door(false, 1_030.0);
        machine.update_door(false, 1_040.0);

        assert!(!machine.sensors.door_open);
        assert_eq!(machine.sensors.door_last_changed, 1_040.0);
        assert_eq!(machine.sensors.door_closed_at, 1_030.0);
    }

    #[test]
    fn disabled_contact_sensors_do_not_update_logical_contact_state() {
        let mut machine = StateMachine::new(contact_disabled_config(), 1_000.0);

        assert_eq!(machine.update_door(true, 1_010.0), None);
        assert_eq!(machine.update_window(true, 1_011.0), None);

        assert!(!machine.sensors.door_open);
        assert_eq!(machine.sensors.door_opened_at, 0.0);
        assert!(!machine.sensors.window_open);
        assert!(!machine.safety_interlock_active());
        assert!(!machine.in_door_open_mode_at(1_400.0));
    }

    #[test]
    fn disabled_contact_sensors_allow_motion_presence_without_door_boundary() {
        let mut machine = StateMachine::new(contact_disabled_config(), 1_000.0);
        machine.sensors.door_open = true;
        machine.sensors.door_last_changed = 2_000.0;

        let transition = machine.update_motion(true, 1_010.0);

        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Away,
                new_state: OccupancyState::Present,
            })
        );
        assert_eq!(machine.state, OccupancyState::Present);
        assert!(!machine.in_door_open_mode_at(2_400.0));
    }

    #[test]
    fn disabled_contact_sensors_do_not_reassert_stale_mac_after_manual_away() {
        let mut machine = StateMachine::new(contact_disabled_config(), 1_000.0);
        assert!(
            machine
                .update_mac_occupancy(1_010.0, true, 1_010.0)
                .is_some()
        );
        assert_eq!(machine.state, OccupancyState::Present);

        machine.set_manual_presence(false, 1_020.0);
        assert_eq!(machine.state, OccupancyState::Away);
        assert_eq!(machine.update_mac_occupancy(1_010.0, true, 1_021.0), None);
        assert_eq!(machine.state, OccupancyState::Away);

        let transition = machine.update_mac_occupancy(1_022.0, true, 1_022.0);
        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Away,
                new_state: OccupancyState::Present,
            })
        );
    }

    #[test]
    fn disabled_contact_sensors_transition_away_after_motion_timeout() {
        let mut machine = StateMachine::new(contact_disabled_config(), 1_000.0);
        assert!(
            machine
                .update_motion(true, 1_010.0)
                .is_some_and(|transition| transition.new_state == OccupancyState::Present)
        );

        let transition = machine.evaluate_at(1_071.0);

        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Present,
                new_state: OccupancyState::Away,
            })
        );
        assert_eq!(machine.state, OccupancyState::Away);
        assert!(!machine.sensors.motion_detected);
        assert!(!machine.presence_signal_active_at(1_071.0));
    }

    #[test]
    fn disabled_contact_sensors_transition_away_when_mac_activity_is_stale() {
        let mut machine = StateMachine::new(contact_disabled_config(), 1_000.0);
        assert!(
            machine
                .update_mac_occupancy(1_010.0, true, 1_010.0)
                .is_some_and(|transition| transition.new_state == OccupancyState::Present)
        );

        let transition = machine.evaluate_at(1_071.0);

        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Present,
                new_state: OccupancyState::Away,
            })
        );
        assert_eq!(machine.state, OccupancyState::Away);
        assert!(!machine.presence_signal_active_at(1_071.0));
    }

    #[test]
    fn disabled_contact_sensors_transition_away_when_external_monitor_disconnects() {
        let mut machine = StateMachine::new(contact_disabled_config(), 1_000.0);
        assert!(
            machine
                .update_mac_occupancy(1_010.0, true, 1_010.0)
                .is_some_and(|transition| transition.new_state == OccupancyState::Present)
        );

        let transition = machine.update_mac_occupancy(1_010.0, false, 1_020.0);

        assert_eq!(
            transition,
            Some(StateTransition {
                old_state: OccupancyState::Present,
                new_state: OccupancyState::Away,
            })
        );
        assert_eq!(machine.state, OccupancyState::Away);
        assert!(!machine.presence_signal_active_at(1_020.0));
    }

    #[test]
    fn safety_interlock_blocks_erv_status_decision() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.update_co2(900, 1_001.0);
        assert!(machine.erv_should_run());

        machine.update_door(true, 1_002.0);
        assert!(machine.safety_interlock_active());
        assert!(!machine.erv_should_run());
    }

    #[test]
    fn present_erv_status_uses_python_greater_than_threshold() {
        let mut machine = StateMachine::new(StateConfig::default(), 1_000.0);
        machine.state = OccupancyState::Present;
        machine.update_co2(2_000, 1_001.0);
        assert!(!machine.erv_should_run());

        machine.update_co2(2_001, 1_002.0);
        assert!(machine.erv_should_run());
    }
}
