//! A Sequoia member's war standing and record, as the map server serves them to members.
//!
//! [`MemberStanding`] is what is true now: the in-game rank on the live roster and the war roles
//! the rank-profile feed reports. [`MemberWarRecord`] is history over a window, read from the
//! backend's stored points. The two are fetched separately so a period change cannot alter the
//! current rank.
//!
//! `war_count` counts wars whose points were stored; `unavailable_war_count` counts wars whose
//! points could not be calculated. Neither is a win, and an unavailable war is not a zero.

use serde::{Deserialize, Serialize};

/// The most recent wars a record lists.
pub const RECENT_WARS: u32 = 8;

/// The longest custom window a member can ask for, in days, both ends included.
pub const MAX_CUSTOM_DAYS: i64 = 400;

/// Why chosen custom days cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomDaysError {
    Reversed,
    Future,
    TooLong,
}

impl CustomDaysError {
    pub fn message(self) -> String {
        match self {
            Self::Reversed => "The first day must be on or before the last".to_string(),
            Self::Future => "The last day can't be after today".to_string(),
            Self::TooLong => format!("Choose at most {MAX_CUSTOM_DAYS} days"),
        }
    }
}

/// Whole UTC days, both inclusive: in order, ending no later than `today`, and at most
/// [`MAX_CUSTOM_DAYS`] of them.
pub fn check_custom_days(
    first: chrono::NaiveDate,
    last: chrono::NaiveDate,
    today: chrono::NaiveDate,
) -> Result<(), CustomDaysError> {
    if first > last {
        return Err(CustomDaysError::Reversed);
    }
    if last > today {
        return Err(CustomDaysError::Future);
    }
    if (last - first).num_days() + 1 > MAX_CUSTOM_DAYS {
        return Err(CustomDaysError::TooLong);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberStanding {
    pub uuid: String,
    pub username: String,
    /// The roster's rank key (`owner` .. `recruit`), as Wynncraft reports it now.
    pub guild_rank: Option<String>,
    pub ranks: RankStanding,
}

/// War roles for everyone on the current roster who holds any, for compact roster rows.
/// A name missing here holds none in the linked feed, which is not proof of anything else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberRosterRoles {
    pub stale: bool,
    pub roles: std::collections::BTreeMap<String, Vec<WarRole>>,
}

/// What the rank-profile feed says about this player.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RankStanding {
    /// The feed has a profile; `stale` when its Discord sync is older than the feed allows.
    Synced { stale: bool, roles: Vec<WarRole> },
    /// The feed has no profile for this UUID. Says nothing about whether the account is linked.
    NoProfile,
    /// The feed could not be asked.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarRole {
    pub key: String,
    pub label: String,
    pub kind: WarRoleKind,
    /// `#RRGGBB`, from the Discord role.
    #[serde(default)]
    pub color: Option<String>,
    /// The role's artwork, when the catalog has one.
    #[serde(default)]
    pub art_url: Option<String>,
}

/// War-relevant role categories, in the order they are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarRoleKind {
    Insignia,
    WarHonour,
    Military,
    MilitarySpecialty,
}

/// The window a record covers, as the server resolved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodKind {
    Week,
    Month,
    Season,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberWarPeriod {
    pub kind: PeriodKind,
    /// RFC 3339, inclusive.
    pub from: String,
    /// RFC 3339, exclusive.
    pub to: String,
    #[serde(default)]
    pub season_id: Option<i32>,
    pub record: MemberWarRecord,
}

/// The backend's answer for one player and window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberWarRecord {
    pub player_uuid: String,
    pub totals: WarTotals,
    /// Totals by the guild each war was attributed to when it happened; `None` guild fields
    /// for wars with no known attribution.
    #[serde(default)]
    pub by_guild: Vec<GuildWarTotals>,
    /// Observed totals of the equal window just before; not evidence that history is complete.
    pub previous: PreviousWindow,
    /// Newest first, bounded by [`RECENT_WARS`].
    #[serde(default)]
    pub recent: Vec<RecentWar>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WarTotals {
    pub war_points: f64,
    pub war_count: u64,
    pub unavailable_war_count: u64,
}

impl WarTotals {
    /// Wars recorded in the window, scored or not.
    pub fn recorded(&self) -> u64 {
        self.war_count + self.unavailable_war_count
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildWarTotals {
    #[serde(default)]
    pub guild_uuid: Option<String>,
    #[serde(default)]
    pub guild_prefix: Option<String>,
    #[serde(default)]
    pub guild_name: Option<String>,
    #[serde(flatten)]
    pub totals: WarTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviousWindow {
    pub from: String,
    pub to: String,
    #[serde(flatten)]
    pub totals: WarTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentWar {
    pub completed_at: String,
    pub territory: String,
    #[serde(default)]
    pub war_points: Option<f64>,
    pub points_status: PointsStatus,
    #[serde(default)]
    pub guild_uuid: Option<String>,
    #[serde(default)]
    pub guild_prefix: Option<String>,
    #[serde(default)]
    pub guild_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointsStatus {
    Scored,
    /// Stored as zero because the war fell outside a season.
    OffSeason,
    /// No points could be calculated for this war.
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_days_allow_exactly_the_limit_both_ends_included() {
        let day = |text: &str| chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap();
        let today = day("2026-11-01");
        let first = day("2025-09-01");
        let spanning = |days: i64| first + chrono::Duration::days(days - 1);
        assert_eq!(check_custom_days(first, spanning(399), today), Ok(()));
        assert_eq!(check_custom_days(first, spanning(400), today), Ok(()));
        assert_eq!(
            check_custom_days(first, spanning(401), today),
            Err(CustomDaysError::TooLong)
        );
        assert_eq!(
            check_custom_days(today, first, today),
            Err(CustomDaysError::Reversed)
        );
        assert_eq!(
            check_custom_days(first, day("2026-11-02"), today),
            Err(CustomDaysError::Future)
        );
    }

    #[test]
    fn a_backend_record_round_trips_with_unknowns_kept_apart_from_zero() {
        let body = r#"{
            "generated_at": "2026-11-01T12:00:00Z",
            "player_uuid": "ee860b7c-9a1d-49cf-9f19-ab673ba0f23b",
            "from": "2026-10-25T12:00:00Z", "to": "2026-11-01T12:00:00Z",
            "totals": {"war_points": 0.0, "war_count": 0, "unavailable_war_count": 2},
            "by_guild": [{"guild_uuid": null, "guild_prefix": null, "guild_name": null,
                          "war_points": 0.0, "war_count": 0, "unavailable_war_count": 2}],
            "previous": {"from": "2026-10-18T12:00:00Z", "to": "2026-10-25T12:00:00Z",
                         "war_points": 0.0, "war_count": 0, "unavailable_war_count": 0},
            "recent": [{"completed_at": "2026-10-30T10:00:00Z", "territory": "Detlas",
                        "war_points": null, "points_status": "unavailable",
                        "guild_uuid": null, "guild_prefix": null, "guild_name": null}]
        }"#;
        let record: MemberWarRecord = serde_json::from_str(body).expect("backend shape");
        assert_eq!(record.totals.recorded(), 2);
        assert_eq!(record.recent[0].war_points, None);
        assert_eq!(record.recent[0].points_status, PointsStatus::Unavailable);
        let echoed = serde_json::to_value(&record).expect("encodes");
        assert!(echoed.get("generated_at").is_none());
        assert_eq!(echoed["by_guild"][0]["unavailable_war_count"], 2);
    }
}
