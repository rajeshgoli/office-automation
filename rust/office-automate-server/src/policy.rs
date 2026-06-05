use std::collections::VecDeque;

use crate::{config::ThresholdsConfig, state::OccupancyState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VentilationSpeed {
    Off,
    Quiet,
    Medium,
    Turbo,
}

impl VentilationSpeed {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Quiet => "quiet",
            Self::Medium => "medium",
            Self::Turbo => "turbo",
        }
    }

    fn priority(self) -> i8 {
        match self {
            Self::Off => 0,
            Self::Quiet => 1,
            Self::Medium => 2,
            Self::Turbo => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvacBandAction {
    PauseHeat,
    ResumeHeat,
    StartCool,
    StopCool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvacMode {
    Off,
    Heat,
    Cool,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ErvPolicyInput {
    pub occupancy: OccupancyState,
    pub door_open: bool,
    pub window_open: bool,
    pub co2_ppm: Option<i64>,
    pub tvoc: Option<i64>,
    pub current_running: bool,
    pub current_speed: VentilationSpeed,
    pub manual_override: Option<VentilationSpeed>,
    pub last_speed_changed_at: Option<f64>,
    pub bypass_dwell: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErvDecision {
    NoChange,
    SuppressedByDwell {
        target_speed: VentilationSpeed,
        reason: String,
    },
    SetSpeed {
        target_speed: VentilationSpeed,
        reason: String,
        bypass_dwell: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AirQualityReading {
    pub co2_ppm: Option<i64>,
    pub tvoc: Option<i64>,
    pub temp_c: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    at: f64,
    value: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ErvPolicyState {
    co2_history: VecDeque<Sample>,
    tvoc_history: VecDeque<Sample>,
    pub plateau_detected: bool,
    pub outdoor_co2_baseline: Option<i64>,
    pub tvoc_away_ventilation_active: bool,
    pub tvoc_baseline: Option<i64>,
    pub tvoc_plateau_detected: bool,
    pub away_start_at: Option<f64>,
    pub room_closed_since: Option<f64>,
    pub room_closed_state_known: bool,
    pub away_stale_flush_active_until: Option<f64>,
    pub away_stale_flush_next_due_at: Option<f64>,
}

impl ErvPolicyState {
    pub fn new(thresholds: &ThresholdsConfig) -> Self {
        Self {
            co2_history: VecDeque::with_capacity(thresholds.co2_history_size),
            tvoc_history: VecDeque::with_capacity(thresholds.tvoc_away_history_size),
            plateau_detected: false,
            outdoor_co2_baseline: None,
            tvoc_away_ventilation_active: false,
            tvoc_baseline: None,
            tvoc_plateau_detected: false,
            away_start_at: None,
            room_closed_since: None,
            room_closed_state_known: false,
            away_stale_flush_active_until: None,
            away_stale_flush_next_due_at: None,
        }
    }

    pub fn record_reading(
        &mut self,
        thresholds: &ThresholdsConfig,
        now: f64,
        reading: AirQualityReading,
    ) {
        if let Some(co2_ppm) = reading.co2_ppm {
            push_bounded(
                &mut self.co2_history,
                thresholds.co2_history_size,
                Sample {
                    at: now,
                    value: co2_ppm,
                },
            );
        }

        if let Some(tvoc) = reading.tvoc {
            push_bounded(
                &mut self.tvoc_history,
                thresholds.tvoc_away_history_size,
                Sample {
                    at: now,
                    value: tvoc,
                },
            );
        }
    }

    pub fn on_occupancy_transition(
        &mut self,
        thresholds: &ThresholdsConfig,
        old_state: OccupancyState,
        new_state: OccupancyState,
        now: f64,
        door_open: bool,
        window_open: bool,
    ) {
        if old_state == new_state {
            return;
        }

        if new_state == OccupancyState::Away {
            self.co2_history.clear();
            self.tvoc_history.clear();
            self.plateau_detected = false;
            self.outdoor_co2_baseline = None;
            self.away_start_at = Some(now);
            self.away_stale_flush_active_until = None;
            self.away_stale_flush_next_due_at = None;
            self.update_room_closed_tracking(now, door_open, window_open, false);
            if thresholds.away_stale_flush_enabled && self.room_closed_since.is_some() {
                let interval = (thresholds.away_stale_flush_interval_hours.max(1) * 3600) as f64;
                let first_due = self.room_closed_since.expect("room closed since set") + interval;
                self.away_stale_flush_next_due_at =
                    Some(if now >= first_due { now } else { first_due });
            }
        }

        if new_state == OccupancyState::Present {
            self.away_start_at = None;
            self.away_stale_flush_active_until = None;
            self.away_stale_flush_next_due_at = None;
            self.tvoc_away_ventilation_active = false;
            self.tvoc_plateau_detected = false;
            self.plateau_detected = false;
            self.outdoor_co2_baseline = None;
        }
    }

    pub fn update_room_closed_tracking(
        &mut self,
        now: f64,
        door_open: bool,
        window_open: bool,
        mark_state_known: bool,
    ) {
        if mark_state_known {
            self.room_closed_state_known = true;
        }

        if !self.room_closed_state_known {
            return;
        }

        if !door_open && !window_open {
            if self.room_closed_since.is_none() {
                self.room_closed_since = Some(now);
            }
            return;
        }

        self.room_closed_since = None;
        self.away_stale_flush_active_until = None;
        self.away_stale_flush_next_due_at = None;
    }

    pub fn decide_erv(
        &mut self,
        thresholds: &ThresholdsConfig,
        input: ErvPolicyInput,
        now: f64,
    ) -> ErvDecision {
        if input.window_open || input.door_open {
            self.tvoc_away_ventilation_active = false;
            if input.current_running {
                return target_decision(
                    thresholds,
                    &input,
                    now,
                    VentilationSpeed::Off,
                    "safety_interlock".to_string(),
                    true,
                );
            }
            return ErvDecision::NoChange;
        }

        if let Some(target_speed) = input.manual_override {
            return target_decision(
                thresholds,
                &input,
                now,
                target_speed,
                "manual_override".to_string(),
                true,
            );
        }

        let co2_critical_on = input
            .co2_ppm
            .is_some_and(|co2| co2 >= thresholds.co2_critical_ppm);
        let co2_critical_off = input.co2_ppm.is_some_and(|co2| {
            co2 < thresholds.co2_critical_ppm - thresholds.co2_critical_hysteresis_ppm
        });
        let co2_needs_refresh = input
            .co2_ppm
            .is_some_and(|co2| co2 > thresholds.co2_refresh_target_ppm);
        let tvoc_needs_clearing = input
            .tvoc
            .is_some_and(|tvoc| tvoc > thresholds.tvoc_away_threshold);
        let tvoc_at_target = input
            .tvoc
            .is_some_and(|tvoc| tvoc <= thresholds.tvoc_away_target);

        match input.occupancy {
            OccupancyState::Present => {
                if co2_critical_on {
                    return target_decision(
                        thresholds,
                        &input,
                        now,
                        VentilationSpeed::Quiet,
                        format!(
                            "present_co2_critical_{}ppm",
                            input.co2_ppm.expect("co2 critical checked")
                        ),
                        true,
                    );
                }

                if input.current_running && input.current_speed == VentilationSpeed::Quiet {
                    if co2_critical_off {
                        return target_decision(
                            thresholds,
                            &input,
                            now,
                            VentilationSpeed::Off,
                            format!(
                                "present_co2_hysteresis_{}ppm",
                                input.co2_ppm.expect("co2 off checked")
                            ),
                            input.bypass_dwell,
                        );
                    }
                    return ErvDecision::NoChange;
                }

                if input.current_running {
                    return target_decision(
                        thresholds,
                        &input,
                        now,
                        VentilationSpeed::Off,
                        "present_air_quality_ok".to_string(),
                        input.bypass_dwell,
                    );
                }

                ErvDecision::NoChange
            }
            OccupancyState::Away => {
                if self.initial_away_settle_active(thresholds, now) {
                    return ErvDecision::NoChange;
                }

                let stale_speed = self.away_stale_flush_speed_if_active(
                    thresholds,
                    now,
                    input.door_open,
                    input.window_open,
                );
                let mut co2_speed = None;
                let mut co2_fallback_turbo = false;
                let mut tvoc_speed = None;
                let latest_tvoc = input.tvoc.or_else(|| self.latest_tvoc());

                if co2_needs_refresh {
                    co2_speed =
                        self.adaptive_co2_speed(thresholds, input.co2_ppm.expect("co2"), now);
                    if co2_speed.is_none() {
                        co2_speed = Some(VentilationSpeed::Turbo);
                        co2_fallback_turbo = true;
                    }
                }

                if tvoc_needs_clearing || self.tvoc_away_ventilation_active {
                    if tvoc_at_target && self.tvoc_away_ventilation_active {
                        self.tvoc_away_ventilation_active = false;
                        self.tvoc_plateau_detected = false;
                    } else if let Some(tvoc) = latest_tvoc {
                        tvoc_speed = self.adaptive_tvoc_speed(thresholds, tvoc, now);
                        if !self.tvoc_away_ventilation_active && tvoc_needs_clearing {
                            self.tvoc_away_ventilation_active = true;
                        }
                    }
                }

                if stale_speed.is_none()
                    && co2_speed == Some(VentilationSpeed::Off)
                    && (tvoc_speed == Some(VentilationSpeed::Off)
                        || !self.tvoc_away_ventilation_active)
                {
                    if input.current_running {
                        let reason = if self.plateau_detected {
                            "co2_plateau"
                        } else {
                            "targets_reached"
                        };
                        return target_decision(
                            thresholds,
                            &input,
                            now,
                            VentilationSpeed::Off,
                            reason.to_string(),
                            input.bypass_dwell,
                        );
                    }
                    return ErvDecision::NoChange;
                }

                let selected = select_away_candidate(co2_speed, tvoc_speed, stale_speed);
                if let Some((source, target_speed)) = selected {
                    if target_speed == VentilationSpeed::Off {
                        return ErvDecision::NoChange;
                    }

                    let reason = match source {
                        AwaySource::Co2 => {
                            let co2 = input.co2_ppm.expect("co2 source selected");
                            if co2_fallback_turbo {
                                format!("away_refresh_CO2={co2}ppm")
                            } else {
                                format!("away_adaptive_{}_CO2={co2}ppm", target_speed.as_str())
                            }
                        }
                        AwaySource::Tvoc => {
                            let tvoc = latest_tvoc.expect("tVOC source selected");
                            format!("away_adaptive_{}_tVOC={tvoc}", target_speed.as_str())
                        }
                        AwaySource::Stale => {
                            format!("away_stale_flush_{}", target_speed.as_str())
                        }
                    };
                    let bypass = input.bypass_dwell
                        || (source == AwaySource::Co2
                            && target_speed == VentilationSpeed::Turbo
                            && self.initial_away_turbo_active(thresholds, now));

                    return target_decision(thresholds, &input, now, target_speed, reason, bypass);
                }

                if !co2_needs_refresh
                    && !self.tvoc_away_ventilation_active
                    && stale_speed.is_none()
                    && input.current_running
                {
                    return target_decision(
                        thresholds,
                        &input,
                        now,
                        VentilationSpeed::Off,
                        "air_quality_ok".to_string(),
                        input.bypass_dwell,
                    );
                }

                if co2_needs_refresh || tvoc_needs_clearing {
                    let trigger = if co2_needs_refresh {
                        format!("CO2={}ppm", input.co2_ppm.expect("co2 trigger"))
                    } else {
                        format!("tVOC={}", input.tvoc.expect("tVOC trigger"))
                    };
                    return target_decision(
                        thresholds,
                        &input,
                        now,
                        VentilationSpeed::Turbo,
                        format!("away_refresh_{trigger}"),
                        input.bypass_dwell || self.initial_away_turbo_active(thresholds, now),
                    );
                }

                ErvDecision::NoChange
            }
        }
    }

    fn co2_rate_of_change(&self) -> Option<f64> {
        rate_of_change(&self.co2_history)
    }

    fn tvoc_rate_of_change(&self) -> Option<f64> {
        rate_of_change(&self.tvoc_history)
    }

    fn latest_tvoc(&self) -> Option<i64> {
        self.tvoc_history.back().map(|sample| sample.value)
    }

    fn detect_co2_plateau(&mut self, thresholds: &ThresholdsConfig) -> bool {
        if !thresholds.co2_plateau_enabled {
            return false;
        }

        let current_co2 = self.co2_history.back().map(|sample| sample.value);
        if self.plateau_detected && self.outdoor_co2_baseline.is_none() {
            self.plateau_detected = false;
        }

        if self.plateau_detected {
            if let (Some(current), Some(baseline)) = (current_co2, self.outdoor_co2_baseline) {
                if current < baseline + thresholds.co2_plateau_release_delta_ppm {
                    return true;
                }
                self.plateau_detected = false;
                self.outdoor_co2_baseline = None;
            } else {
                return true;
            }
        }

        let min_readings = 20.max((thresholds.co2_plateau_window_minutes * 2) as usize);
        if self.co2_history.len() < min_readings {
            return false;
        }

        let current_co2 = current_co2.expect("history has readings");
        if current_co2 > thresholds.co2_plateau_min_co2 {
            return false;
        }

        let Some(rate) = self.co2_rate_of_change() else {
            return false;
        };

        if rate.abs() < thresholds.co2_plateau_rate_threshold {
            self.outdoor_co2_baseline = Some(current_co2);
            self.plateau_detected = true;
            return true;
        }

        false
    }

    fn adaptive_co2_speed(
        &mut self,
        thresholds: &ThresholdsConfig,
        _co2: i64,
        now: f64,
    ) -> Option<VentilationSpeed> {
        if !thresholds.co2_adaptive_speed_enabled {
            return None;
        }

        if self.detect_co2_plateau(thresholds) {
            return Some(VentilationSpeed::Off);
        }

        if self.initial_away_turbo_active(thresholds, now) {
            return Some(VentilationSpeed::Turbo);
        }

        let rate = self.co2_rate_of_change()?;
        let abs_rate = rate.abs();
        if abs_rate > thresholds.co2_rate_turbo_threshold {
            Some(VentilationSpeed::Turbo)
        } else if abs_rate > thresholds.co2_rate_medium_threshold {
            Some(VentilationSpeed::Medium)
        } else if abs_rate > thresholds.co2_rate_quiet_threshold {
            Some(VentilationSpeed::Quiet)
        } else {
            Some(VentilationSpeed::Quiet)
        }
    }

    fn detect_tvoc_plateau(&mut self, thresholds: &ThresholdsConfig) -> bool {
        if !thresholds.tvoc_away_enabled || self.tvoc_history.len() < 20 {
            return false;
        }

        let current_tvoc = self
            .tvoc_history
            .back()
            .expect("history has readings")
            .value;
        if current_tvoc > thresholds.tvoc_away_target + 20 {
            return false;
        }

        let Some(rate) = self.tvoc_rate_of_change() else {
            return false;
        };
        if rate.abs() < thresholds.tvoc_plateau_rate_threshold {
            self.tvoc_baseline = Some(current_tvoc);
            return true;
        }

        false
    }

    fn adaptive_tvoc_speed(
        &mut self,
        thresholds: &ThresholdsConfig,
        _tvoc: i64,
        now: f64,
    ) -> Option<VentilationSpeed> {
        if !thresholds.tvoc_away_enabled {
            return None;
        }

        if self.initial_away_turbo_active(thresholds, now) {
            return Some(VentilationSpeed::Turbo);
        }

        if self.detect_tvoc_plateau(thresholds) {
            self.tvoc_plateau_detected = true;
            return Some(VentilationSpeed::Off);
        }

        let rate = self.tvoc_rate_of_change()?;
        let abs_rate = rate.abs();
        if abs_rate > thresholds.tvoc_rate_turbo_threshold {
            Some(VentilationSpeed::Turbo)
        } else if abs_rate > thresholds.tvoc_rate_medium_threshold {
            Some(VentilationSpeed::Medium)
        } else if abs_rate > thresholds.tvoc_rate_quiet_threshold {
            Some(VentilationSpeed::Quiet)
        } else {
            Some(VentilationSpeed::Quiet)
        }
    }

    fn away_stale_flush_speed_if_active(
        &mut self,
        thresholds: &ThresholdsConfig,
        now: f64,
        door_open: bool,
        window_open: bool,
    ) -> Option<VentilationSpeed> {
        if !thresholds.away_stale_flush_enabled {
            self.away_stale_flush_active_until = None;
            self.away_stale_flush_next_due_at = None;
            return None;
        }

        self.update_room_closed_tracking(now, door_open, window_open, false);
        self.room_closed_since?;

        let interval = (thresholds.away_stale_flush_interval_hours.max(1) * 3600) as f64;
        let duration = (thresholds.away_stale_flush_duration_minutes.max(1) * 60) as f64;

        if self.away_stale_flush_next_due_at.is_none() {
            let first_due = self.room_closed_since.expect("room closed") + interval;
            self.away_stale_flush_next_due_at =
                Some(if now >= first_due { now } else { first_due });
        }

        if self
            .away_stale_flush_active_until
            .is_some_and(|active_until| now >= active_until)
        {
            self.away_stale_flush_active_until = None;
        }

        if self.away_stale_flush_active_until.is_none()
            && self
                .away_stale_flush_next_due_at
                .is_some_and(|next_due| now >= next_due)
        {
            self.away_stale_flush_active_until = Some(now + duration);
            self.away_stale_flush_next_due_at = Some(now + interval);
        }

        if self
            .away_stale_flush_active_until
            .is_some_and(|active_until| now < active_until)
        {
            return Some(stale_flush_speed(thresholds));
        }

        None
    }

    fn initial_away_settle_active(&self, thresholds: &ThresholdsConfig, now: f64) -> bool {
        thresholds.min_away_seconds_before_erv > 0
            && self.away_start_at.is_some_and(|started_at| {
                now - started_at < thresholds.min_away_seconds_before_erv as f64
            })
    }

    fn initial_away_turbo_active(&self, thresholds: &ThresholdsConfig, now: f64) -> bool {
        self.away_start_at.is_some_and(|started_at| {
            (now - started_at) / 60.0 < thresholds.co2_turbo_duration_minutes as f64
        })
    }
}

pub fn get_hvac_band_action(
    temp_f: Option<f64>,
    hvac_mode: HvacMode,
    temp_band_mode: Option<HvacMode>,
    state: OccupancyState,
    erv_running: bool,
    min_temp_f: f64,
    within_occupancy_hours: bool,
    heat_off_temp_f: f64,
    heat_on_temp_f: f64,
    cool_on_temp_f: f64,
    cool_off_temp_f: f64,
) -> Option<HvacBandAction> {
    let temp_f = temp_f?;

    if hvac_mode == HvacMode::Heat && temp_f >= heat_off_temp_f {
        return Some(HvacBandAction::PauseHeat);
    }

    if hvac_mode == HvacMode::Cool && temp_f <= cool_off_temp_f {
        return Some(HvacBandAction::StopCool);
    }

    if hvac_mode != HvacMode::Off {
        return None;
    }

    if temp_band_mode == Some(HvacMode::Heat) && temp_f <= heat_on_temp_f {
        if state == OccupancyState::Away {
            if erv_running && temp_f > min_temp_f {
                return None;
            }
            if !within_occupancy_hours {
                return None;
            }
        }

        return Some(HvacBandAction::ResumeHeat);
    }

    if state == OccupancyState::Present && temp_f > cool_on_temp_f {
        return Some(HvacBandAction::StartCool);
    }

    None
}

fn target_decision(
    thresholds: &ThresholdsConfig,
    input: &ErvPolicyInput,
    now: f64,
    target_speed: VentilationSpeed,
    reason: String,
    bypass_dwell: bool,
) -> ErvDecision {
    if target_speed == VentilationSpeed::Off && !input.current_running {
        return ErvDecision::NoChange;
    }
    if target_speed != VentilationSpeed::Off
        && input.current_running
        && input.current_speed == target_speed
    {
        return ErvDecision::NoChange;
    }

    if !bypass_dwell
        && thresholds.erv_min_dwell_seconds > 0
        && input
            .last_speed_changed_at
            .is_some_and(|changed_at| now - changed_at < thresholds.erv_min_dwell_seconds as f64)
    {
        return ErvDecision::SuppressedByDwell {
            target_speed,
            reason,
        };
    }

    ErvDecision::SetSpeed {
        target_speed,
        reason,
        bypass_dwell,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AwaySource {
    Co2,
    Tvoc,
    Stale,
}

fn select_away_candidate(
    co2_speed: Option<VentilationSpeed>,
    tvoc_speed: Option<VentilationSpeed>,
    stale_speed: Option<VentilationSpeed>,
) -> Option<(AwaySource, VentilationSpeed)> {
    [
        (AwaySource::Co2, co2_speed, 3_i8),
        (AwaySource::Tvoc, tvoc_speed, 2_i8),
        (AwaySource::Stale, stale_speed, 1_i8),
    ]
    .into_iter()
    .filter_map(|(source, speed, source_priority)| {
        speed.map(|speed| (source, speed, speed.priority(), source_priority))
    })
    .max_by_key(|(_, _, speed_priority, source_priority)| (*speed_priority, *source_priority))
    .map(|(source, speed, _, _)| (source, speed))
}

fn push_bounded(queue: &mut VecDeque<Sample>, max_len: usize, sample: Sample) {
    let max_len = max_len.max(1);
    if queue.len() >= max_len {
        queue.pop_front();
    }
    queue.push_back(sample);
}

fn rate_of_change(queue: &VecDeque<Sample>) -> Option<f64> {
    let oldest = queue.front()?;
    let newest = queue.back()?;
    let minutes = (newest.at - oldest.at) / 60.0;
    if minutes == 0.0 {
        return None;
    }
    Some((newest.value - oldest.value) as f64 / minutes)
}

fn stale_flush_speed(thresholds: &ThresholdsConfig) -> VentilationSpeed {
    match thresholds
        .away_stale_flush_speed
        .to_ascii_lowercase()
        .as_str()
    {
        "quiet" => VentilationSpeed::Quiet,
        "turbo" => VentilationSpeed::Turbo,
        _ => VentilationSpeed::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn away_input(co2_ppm: Option<i64>, tvoc: Option<i64>) -> ErvPolicyInput {
        ErvPolicyInput {
            occupancy: OccupancyState::Away,
            door_open: false,
            window_open: false,
            co2_ppm,
            tvoc,
            current_running: false,
            current_speed: VentilationSpeed::Off,
            manual_override: None,
            last_speed_changed_at: None,
            bypass_dwell: false,
        }
    }

    #[test]
    fn safety_interlock_turns_running_erv_off_with_dwell_bypass() {
        let thresholds = ThresholdsConfig::default();
        let mut policy = ErvPolicyState::new(&thresholds);
        let decision = policy.decide_erv(
            &thresholds,
            ErvPolicyInput {
                door_open: true,
                current_running: true,
                current_speed: VentilationSpeed::Quiet,
                last_speed_changed_at: Some(1_000.0),
                ..away_input(Some(700), None)
            },
            1_001.0,
        );

        assert_eq!(
            decision,
            ErvDecision::SetSpeed {
                target_speed: VentilationSpeed::Off,
                reason: "safety_interlock".to_string(),
                bypass_dwell: true,
            }
        );
    }

    #[test]
    fn manual_override_returns_target_without_device_write() {
        let thresholds = ThresholdsConfig::default();
        let mut policy = ErvPolicyState::new(&thresholds);
        let decision = policy.decide_erv(
            &thresholds,
            ErvPolicyInput {
                manual_override: Some(VentilationSpeed::Turbo),
                last_speed_changed_at: Some(1_000.0),
                ..away_input(Some(450), None)
            },
            1_001.0,
        );

        assert_eq!(
            decision,
            ErvDecision::SetSpeed {
                target_speed: VentilationSpeed::Turbo,
                reason: "manual_override".to_string(),
                bypass_dwell: true,
            }
        );
    }

    #[test]
    fn present_mode_uses_co2_hysteresis_and_ignores_tvoc() {
        let thresholds = ThresholdsConfig::default();
        let mut policy = ErvPolicyState::new(&thresholds);
        let decision = policy.decide_erv(
            &thresholds,
            ErvPolicyInput {
                occupancy: OccupancyState::Present,
                co2_ppm: Some(2_000),
                tvoc: Some(500),
                current_running: false,
                current_speed: VentilationSpeed::Off,
                manual_override: None,
                door_open: false,
                window_open: false,
                last_speed_changed_at: Some(1_000.0),
                bypass_dwell: false,
            },
            1_001.0,
        );

        assert_eq!(
            decision,
            ErvDecision::SetSpeed {
                target_speed: VentilationSpeed::Quiet,
                reason: "present_co2_critical_2000ppm".to_string(),
                bypass_dwell: true,
            }
        );

        let decision = policy.decide_erv(
            &thresholds,
            ErvPolicyInput {
                occupancy: OccupancyState::Present,
                co2_ppm: Some(1_799),
                tvoc: Some(500),
                current_running: true,
                current_speed: VentilationSpeed::Quiet,
                manual_override: None,
                door_open: false,
                window_open: false,
                last_speed_changed_at: Some(1_000.0),
                bypass_dwell: true,
            },
            1_001.0,
        );

        assert_eq!(
            decision,
            ErvDecision::SetSpeed {
                target_speed: VentilationSpeed::Off,
                reason: "present_co2_hysteresis_1799ppm".to_string(),
                bypass_dwell: true,
            }
        );
    }

    #[test]
    fn away_settle_window_holds_prior_erv_state() {
        let thresholds = ThresholdsConfig {
            min_away_seconds_before_erv: 60,
            ..ThresholdsConfig::default()
        };
        let mut policy = ErvPolicyState::new(&thresholds);
        policy.away_start_at = Some(1_000.0);

        let decision = policy.decide_erv(&thresholds, away_input(Some(900), None), 1_030.0);

        assert_eq!(decision, ErvDecision::NoChange);
    }

    #[test]
    fn away_high_co2_falls_back_to_turbo_until_adaptive_history_exists() {
        let thresholds = ThresholdsConfig {
            min_away_seconds_before_erv: 0,
            away_stale_flush_enabled: false,
            ..ThresholdsConfig::default()
        };
        let mut policy = ErvPolicyState::new(&thresholds);
        policy.away_start_at = Some(0.0);

        let decision = policy.decide_erv(&thresholds, away_input(Some(700), None), 1_901.0);

        assert_eq!(
            decision,
            ErvDecision::SetSpeed {
                target_speed: VentilationSpeed::Turbo,
                reason: "away_refresh_CO2=700ppm".to_string(),
                bypass_dwell: false,
            }
        );
    }

    #[test]
    fn co2_plateau_latches_until_baseline_delta_release() {
        let thresholds = ThresholdsConfig {
            min_away_seconds_before_erv: 0,
            co2_plateau_release_delta_ppm: 100,
            away_stale_flush_enabled: false,
            ..ThresholdsConfig::default()
        };
        let mut policy = ErvPolicyState::new(&thresholds);
        policy.away_start_at = Some(0.0);
        for index in 0..24 {
            let value = [512, 513, 511, 512][index % 4];
            policy.record_reading(
                &thresholds,
                index as f64 * 30.0,
                AirQualityReading {
                    co2_ppm: Some(value),
                    tvoc: None,
                    temp_c: None,
                },
            );
        }

        let decision = policy.decide_erv(
            &thresholds,
            ErvPolicyInput {
                current_running: true,
                current_speed: VentilationSpeed::Quiet,
                ..away_input(Some(512), None)
            },
            720.0,
        );
        assert_eq!(
            decision,
            ErvDecision::SetSpeed {
                target_speed: VentilationSpeed::Off,
                reason: "co2_plateau".to_string(),
                bypass_dwell: false,
            }
        );
        assert!(policy.plateau_detected);
        assert_eq!(policy.outdoor_co2_baseline, Some(512));

        policy.record_reading(
            &thresholds,
            750.0,
            AirQualityReading {
                co2_ppm: Some(612),
                tvoc: None,
                temp_c: None,
            },
        );
        assert!(!policy.detect_co2_plateau(&thresholds));
        assert!(!policy.plateau_detected);
    }

    #[test]
    fn away_tvoc_high_falls_back_to_turbo_when_adaptive_history_is_missing() {
        let thresholds = ThresholdsConfig {
            min_away_seconds_before_erv: 0,
            away_stale_flush_enabled: false,
            ..ThresholdsConfig::default()
        };
        let mut policy = ErvPolicyState::new(&thresholds);
        policy.away_start_at = Some(0.0);

        let decision = policy.decide_erv(&thresholds, away_input(Some(450), Some(250)), 1_900.0);

        assert_eq!(
            decision,
            ErvDecision::SetSpeed {
                target_speed: VentilationSpeed::Turbo,
                reason: "away_refresh_tVOC=250".to_string(),
                bypass_dwell: false,
            }
        );
        assert!(policy.tvoc_away_ventilation_active);
    }

    #[test]
    fn active_tvoc_ventilation_tolerates_missing_tvoc_reading() {
        let thresholds = ThresholdsConfig {
            min_away_seconds_before_erv: 0,
            away_stale_flush_enabled: false,
            ..ThresholdsConfig::default()
        };
        let mut policy = ErvPolicyState::new(&thresholds);
        policy.away_start_at = Some(0.0);
        policy.record_reading(
            &thresholds,
            1_900.0,
            AirQualityReading {
                co2_ppm: Some(450),
                tvoc: Some(250),
                temp_c: None,
            },
        );

        let decision = policy.decide_erv(&thresholds, away_input(Some(450), Some(250)), 1_900.0);
        assert_eq!(
            decision,
            ErvDecision::SetSpeed {
                target_speed: VentilationSpeed::Turbo,
                reason: "away_refresh_tVOC=250".to_string(),
                bypass_dwell: false,
            }
        );
        assert!(policy.tvoc_away_ventilation_active);

        let decision = policy.decide_erv(
            &thresholds,
            ErvPolicyInput {
                current_running: true,
                current_speed: VentilationSpeed::Turbo,
                tvoc: None,
                ..away_input(Some(450), None)
            },
            1_930.0,
        );

        assert_eq!(decision, ErvDecision::NoChange);
        assert!(policy.tvoc_away_ventilation_active);
    }

    #[test]
    fn stale_flush_runs_at_configured_speed_but_loses_to_co2_turbo() {
        let thresholds = ThresholdsConfig {
            min_away_seconds_before_erv: 0,
            away_stale_flush_enabled: true,
            away_stale_flush_interval_hours: 8,
            away_stale_flush_duration_minutes: 30,
            away_stale_flush_speed: "medium".to_string(),
            ..ThresholdsConfig::default()
        };
        let mut policy = ErvPolicyState::new(&thresholds);
        policy.room_closed_state_known = true;
        policy.room_closed_since = Some(0.0);
        policy.away_stale_flush_next_due_at = Some(100.0);

        let decision = policy.decide_erv(&thresholds, away_input(Some(450), Some(20)), 101.0);
        assert_eq!(
            decision,
            ErvDecision::SetSpeed {
                target_speed: VentilationSpeed::Medium,
                reason: "away_stale_flush_medium".to_string(),
                bypass_dwell: false,
            }
        );

        let decision = policy.decide_erv(&thresholds, away_input(Some(700), Some(20)), 102.0);
        assert_eq!(
            decision,
            ErvDecision::SetSpeed {
                target_speed: VentilationSpeed::Turbo,
                reason: "away_refresh_CO2=700ppm".to_string(),
                bypass_dwell: false,
            }
        );
    }

    #[test]
    fn dwell_suppresses_non_bypassed_speed_change_without_side_effects() {
        let thresholds = ThresholdsConfig {
            erv_min_dwell_seconds: 180,
            min_away_seconds_before_erv: 0,
            away_stale_flush_enabled: false,
            ..ThresholdsConfig::default()
        };
        let mut policy = ErvPolicyState::new(&thresholds);
        let decision = policy.decide_erv(
            &thresholds,
            ErvPolicyInput {
                current_running: true,
                current_speed: VentilationSpeed::Quiet,
                last_speed_changed_at: Some(1_000.0),
                ..away_input(Some(450), None)
            },
            1_010.0,
        );

        assert_eq!(
            decision,
            ErvDecision::SuppressedByDwell {
                target_speed: VentilationSpeed::Off,
                reason: "air_quality_ok".to_string(),
            }
        );
    }

    #[test]
    fn hvac_band_action_matches_python_helper() {
        assert_eq!(
            get_hvac_band_action(
                Some(75.2),
                HvacMode::Heat,
                None,
                OccupancyState::Present,
                false,
                68.0,
                true,
                75.0,
                71.0,
                81.0,
                78.0,
            ),
            Some(HvacBandAction::PauseHeat)
        );
        assert_eq!(
            get_hvac_band_action(
                Some(70.0),
                HvacMode::Off,
                Some(HvacMode::Heat),
                OccupancyState::Away,
                true,
                68.0,
                true,
                75.0,
                71.0,
                81.0,
                78.0,
            ),
            None
        );
        assert_eq!(
            get_hvac_band_action(
                Some(81.1),
                HvacMode::Off,
                None,
                OccupancyState::Present,
                false,
                68.0,
                true,
                75.0,
                71.0,
                81.0,
                78.0,
            ),
            Some(HvacBandAction::StartCool)
        );
        assert_eq!(
            get_hvac_band_action(
                Some(78.0),
                HvacMode::Cool,
                None,
                OccupancyState::Present,
                false,
                68.0,
                true,
                75.0,
                71.0,
                81.0,
                78.0,
            ),
            Some(HvacBandAction::StopCool)
        );
    }
}
