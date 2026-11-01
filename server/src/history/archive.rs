//! Compact canonical archive. A report keeps its columns, a guild dictionary id, its static
//! fields once per distinct version and its runtime in a typed binary form. Compaction is
//! accepted only when the stored parts reproduce the original payload value exactly;
//! otherwise the raw payload is kept.

use chrono::{DateTime, SecondsFormat, Utc};
use sequoia_shared::{
    CanonicalTerritoryUpdate, DataProvenance, GuildRef, Resources, TerritoryRuntimeData,
    VisibilityClass,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::unpack_color;
use super::writer::GuildKey;

const RUNTIME_FORMAT: u8 = 1;

/// One accepted canonical report, in the form it is stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub id: Option<i64>,
    pub territory: String,
    pub received_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    pub confidence: f64,
    pub visibility: i16,
    pub source: String,
    pub reporter_count: i32,
    pub idempotency_key: Option<String>,
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Compact {
        guild: Option<GuildKey>,
        acquired_at: Option<DateTime<Utc>>,
        statics: Option<Value>,
        runtime: Option<Vec<u8>>,
    },
    Raw(Value),
}

pub struct Resolved<'a> {
    pub report: &'a Report,
    pub territory_id: i16,
    pub guild_id: Option<i32>,
}

pub fn visibility_code(visibility: &str) -> i16 {
    match visibility {
        "public" => 0,
        "guild_opt_in" => 1,
        _ => -1,
    }
}

pub fn visibility_name(code: i16) -> &'static str {
    match code {
        0 => "public",
        1 => "guild_opt_in",
        _ => "unknown",
    }
}

impl Report {
    /// The columns the legacy archive derived from an accepted update.
    pub fn from_update(update: &CanonicalTerritoryUpdate, received_at: DateTime<Utc>) -> Self {
        let provenance = update
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.provenance.as_ref());
        let payload = serde_json::to_value(update).unwrap_or(Value::Null);
        Self::from_payload(
            payload,
            Columns {
                id: None,
                territory: update.territory.clone(),
                received_at,
                observed_at: provenance
                    .and_then(|value| DateTime::parse_from_rfc3339(&value.observed_at).ok())
                    .map_or(received_at, |value| value.with_timezone(&Utc)),
                confidence: provenance.map_or(1.0, |value| f64::from(value.confidence)),
                visibility: provenance.map_or("public", |value| match value.visibility {
                    VisibilityClass::Public => "public",
                    VisibilityClass::GuildOptIn => "guild_opt_in",
                }),
                source: provenance
                    .map(|value| value.source.clone())
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "unknown".to_string()),
                reporter_count: provenance.map_or(0, |value| i32::from(value.reporter_count)),
                idempotency_key: update.idempotency_key.clone(),
            },
        )
    }

    pub fn from_payload(payload: Value, columns: Columns<'_>) -> Self {
        let body = compact(&payload).unwrap_or(Body::Raw(payload));
        Self {
            id: columns.id,
            territory: columns.territory,
            received_at: columns.received_at,
            observed_at: columns.observed_at,
            confidence: columns.confidence,
            visibility: visibility_code(columns.visibility),
            source: columns.source,
            reporter_count: columns.reporter_count,
            idempotency_key: columns.idempotency_key,
            body,
        }
    }

    pub fn guild(&self) -> Option<&GuildKey> {
        match &self.body {
            Body::Compact { guild, .. } => guild.as_ref(),
            Body::Raw(_) => None,
        }
    }
}

pub struct Columns<'a> {
    pub id: Option<i64>,
    pub territory: String,
    pub received_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    pub confidence: f64,
    pub visibility: &'a str,
    pub source: String,
    pub reporter_count: i32,
    pub idempotency_key: Option<String>,
}

fn compact(payload: &Value) -> Option<Body> {
    let update: CanonicalTerritoryUpdate = serde_json::from_value(payload.clone()).ok()?;
    let acquired_at = match update.acquired.as_deref() {
        Some(raw) => Some(DateTime::parse_from_rfc3339(raw).ok()?.with_timezone(&Utc)),
        None => None,
    };
    let statics =
        (update.location.is_some() || update.resources.is_some() || update.connections.is_some())
            .then(|| {
                serde_json::json!({
                    "location": update.location,
                    "resources": update.resources,
                    "connections": update.connections,
                })
            });
    let body = Body::Compact {
        guild: update.guild.as_ref().map(GuildKey::from),
        acquired_at,
        statics,
        runtime: update.runtime.as_ref().map(encode_runtime),
    };
    let rebuilt = rebuild(&update.territory, update.idempotency_key.as_deref(), &body)?;
    (serde_json::to_value(rebuilt).ok()? == *payload).then_some(body)
}

/// The payload a compact body stands for.
pub fn rebuild(
    territory: &str,
    idempotency_key: Option<&str>,
    body: &Body,
) -> Option<CanonicalTerritoryUpdate> {
    let Body::Compact {
        guild,
        acquired_at,
        statics,
        runtime,
    } = body
    else {
        return None;
    };
    let field = |name: &str| statics.as_ref().and_then(|value| value.get(name)).cloned();
    Some(CanonicalTerritoryUpdate {
        territory: territory.to_string(),
        guild: guild.as_ref().map(|key| GuildRef {
            uuid: key.uuid.clone(),
            name: key.name.clone(),
            prefix: key.prefix.clone(),
            color: unpack_color(key.color),
        }),
        acquired: acquired_at.map(|at| at.to_rfc3339()),
        location: field("location").and_then(|value| serde_json::from_value(value).ok()),
        resources: field("resources").and_then(|value| serde_json::from_value(value).ok()),
        connections: field("connections").and_then(|value| serde_json::from_value(value).ok()),
        runtime: match runtime {
            Some(bytes) => Some(decode_runtime(bytes)?),
            None => None,
        },
        idempotency_key: idempotency_key.map(str::to_string),
    })
}

pub fn static_digest(statics: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(statics).unwrap_or_default();
    Sha256::digest(bytes)[..16].to_vec()
}

#[derive(Serialize, Deserialize)]
struct RuntimeV1 {
    headquarters: Option<bool>,
    headquarters_territory: Option<String>,
    held_resources: Option<Resources>,
    production_rates: Option<Resources>,
    storage_capacity: Option<Resources>,
    treasury: Option<String>,
    defense_tier: Option<String>,
    contested: Option<bool>,
    active_war: Option<bool>,
    extra_scrapes: Option<String>,
    provenance: Option<ProvenanceV1>,
}

#[derive(Serialize, Deserialize)]
struct ProvenanceV1 {
    source: String,
    guild_opt_in: bool,
    confidence: f32,
    reporter_count: u16,
    observed_at: Instant,
    menu_season_id: Option<i32>,
    menu_captured_territories: Option<u16>,
    menu_sr_per_hour: Option<i32>,
    menu_observed_at: Option<String>,
}

/// A reporter's timestamp: nanoseconds when that reproduces its text exactly.
#[derive(Serialize, Deserialize)]
enum Instant {
    Nanos(i64),
    Text(String),
}

impl Instant {
    fn new(text: &str) -> Self {
        DateTime::parse_from_rfc3339(text)
            .ok()
            .and_then(|at| at.with_timezone(&Utc).timestamp_nanos_opt())
            .filter(|nanos| instant_text(*nanos) == text)
            .map_or_else(|| Self::Text(text.to_string()), Self::Nanos)
    }

    fn text(&self) -> String {
        match self {
            Self::Nanos(nanos) => instant_text(*nanos),
            Self::Text(text) => text.clone(),
        }
    }
}

fn instant_text(nanos: i64) -> String {
    DateTime::from_timestamp_nanos(nanos).to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn encode_runtime(runtime: &TerritoryRuntimeData) -> Vec<u8> {
    let compact = RuntimeV1 {
        headquarters: runtime.headquarters,
        headquarters_territory: runtime.headquarters_territory.clone(),
        held_resources: runtime.held_resources.clone(),
        production_rates: runtime.production_rates.clone(),
        storage_capacity: runtime.storage_capacity.clone(),
        treasury: runtime.treasury.clone(),
        defense_tier: runtime.defense_tier.clone(),
        contested: runtime.contested,
        active_war: runtime.active_war,
        extra_scrapes: runtime
            .extra_scrapes
            .as_ref()
            .and_then(|extra| serde_json::to_string(extra).ok()),
        provenance: runtime.provenance.as_ref().map(|provenance| ProvenanceV1 {
            source: provenance.source.clone(),
            guild_opt_in: provenance.visibility == VisibilityClass::GuildOptIn,
            confidence: provenance.confidence,
            reporter_count: provenance.reporter_count,
            observed_at: Instant::new(&provenance.observed_at),
            menu_season_id: provenance.menu_season_id,
            menu_captured_territories: provenance.menu_captured_territories,
            menu_sr_per_hour: provenance.menu_sr_per_hour,
            menu_observed_at: provenance.menu_observed_at.clone(),
        }),
    };
    let mut bytes = vec![RUNTIME_FORMAT];
    bytes.extend(postcard::to_allocvec(&compact).unwrap_or_default());
    bytes
}

fn decode_runtime(bytes: &[u8]) -> Option<TerritoryRuntimeData> {
    let (&format, body) = bytes.split_first()?;
    if format != RUNTIME_FORMAT {
        return None;
    }
    let compact: RuntimeV1 = postcard::from_bytes(body).ok()?;
    Some(TerritoryRuntimeData {
        headquarters: compact.headquarters,
        headquarters_territory: compact.headquarters_territory,
        held_resources: compact.held_resources,
        production_rates: compact.production_rates,
        storage_capacity: compact.storage_capacity,
        treasury: compact.treasury,
        defense_tier: compact.defense_tier,
        contested: compact.contested,
        active_war: compact.active_war,
        extra_scrapes: match compact.extra_scrapes {
            Some(text) => Some(serde_json::from_str(&text).ok()?),
            None => None,
        },
        provenance: compact.provenance.map(|provenance| DataProvenance {
            source: provenance.source,
            visibility: if provenance.guild_opt_in {
                VisibilityClass::GuildOptIn
            } else {
                VisibilityClass::Public
            },
            confidence: provenance.confidence,
            reporter_count: provenance.reporter_count,
            observed_at: provenance.observed_at.text(),
            menu_season_id: provenance.menu_season_id,
            menu_captured_territories: provenance.menu_captured_territories,
            menu_sr_per_hour: provenance.menu_sr_per_hour,
            menu_observed_at: provenance.menu_observed_at,
        }),
    })
}

/// The stored payload of a report, exactly as the legacy archive held it.
pub fn payload(report: &Report) -> Option<Value> {
    match &report.body {
        Body::Raw(value) => Some(value.clone()),
        body => serde_json::to_value(rebuild(
            &report.territory,
            report.idempotency_key.as_deref(),
            body,
        )?)
        .ok(),
    }
}

fn compact_parts(report: &Report) -> (Option<DateTime<Utc>>, Option<&[u8]>) {
    match &report.body {
        Body::Compact {
            acquired_at,
            runtime,
            ..
        } => (*acquired_at, runtime.as_deref()),
        Body::Raw(_) => (None, None),
    }
}

/// Store reports. Dictionary rows are added only when missing, so repeated sources and
/// static versions draw no new identity values.
pub async fn insert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    reports: &[Resolved<'_>],
) -> Result<(), sqlx::Error> {
    if reports.is_empty() {
        return Ok(());
    }
    let mut sources: Vec<&str> = reports.iter().map(|r| r.report.source.as_str()).collect();
    sources.sort_unstable();
    sources.dedup();
    sqlx::query(
        "INSERT INTO canonical_source (name) SELECT n FROM UNNEST($1::text[]) AS n \
         WHERE NOT EXISTS (SELECT 1 FROM canonical_source s WHERE s.name = n) \
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(&sources)
    .execute(&mut **tx)
    .await?;

    let mut digests = Vec::new();
    let mut statics = Vec::new();
    let mut static_digest_of = Vec::with_capacity(reports.len());
    for resolved in reports {
        let digest = match &resolved.report.body {
            Body::Compact {
                statics: Some(fields),
                ..
            } => {
                let digest = static_digest(fields);
                if !digests.contains(&digest) {
                    digests.push(digest.clone());
                    statics.push(fields.clone());
                }
                Some(digest)
            }
            _ => None,
        };
        static_digest_of.push(digest);
    }
    sqlx::query(
        "INSERT INTO canonical_static (digest, fields) \
         SELECT * FROM UNNEST($1::bytea[], $2::jsonb[]) AS x(digest, fields) \
         WHERE NOT EXISTS (SELECT 1 FROM canonical_static s WHERE s.digest = x.digest) \
         ON CONFLICT (digest) DO NOTHING",
    )
    .bind(&digests)
    .bind(&statics)
    .execute(&mut **tx)
    .await?;

    let acquired: Vec<Option<DateTime<Utc>>> =
        reports.iter().map(|r| compact_parts(r.report).0).collect();
    let runtime: Vec<Option<&[u8]>> = reports.iter().map(|r| compact_parts(r.report).1).collect();
    let raw: Vec<Option<&Value>> = reports
        .iter()
        .map(|r| match &r.report.body {
            Body::Raw(value) => Some(value),
            Body::Compact { .. } => None,
        })
        .collect();
    sqlx::query(
        "INSERT INTO canonical_report (id, received_at, observed_at, acquired_at, confidence, \
             guild_id, static_id, reporter_count, territory_id, source_id, visibility, \
             idempotency_key, runtime, raw_payload) \
         SELECT coalesce(r.id, nextval('canonical_report_id_seq')), r.received_at, \
             r.observed_at, r.acquired_at, r.confidence, r.guild_id, s.id, r.reporter_count, \
             r.territory_id, src.id, r.visibility, r.idempotency_key, r.runtime, r.raw_payload \
         FROM UNNEST($1::int8[], $2::timestamptz[], $3::timestamptz[], $4::timestamptz[], \
             $5::float8[], $6::int4[], $7::bytea[], $8::int4[], $9::int2[], $10::text[], \
             $11::int2[], $12::text[], $13::bytea[], $14::jsonb[]) \
             AS r(id, received_at, observed_at, acquired_at, confidence, guild_id, digest, \
                  reporter_count, territory_id, source, visibility, idempotency_key, runtime, \
                  raw_payload) \
         JOIN canonical_source src ON src.name = r.source \
         LEFT JOIN canonical_static s ON s.digest = r.digest \
         ON CONFLICT DO NOTHING",
    )
    .bind(reports.iter().map(|r| r.report.id).collect::<Vec<_>>())
    .bind(
        reports
            .iter()
            .map(|r| r.report.received_at)
            .collect::<Vec<_>>(),
    )
    .bind(
        reports
            .iter()
            .map(|r| r.report.observed_at)
            .collect::<Vec<_>>(),
    )
    .bind(acquired)
    .bind(
        reports
            .iter()
            .map(|r| r.report.confidence)
            .collect::<Vec<_>>(),
    )
    .bind(reports.iter().map(|r| r.guild_id).collect::<Vec<_>>())
    .bind(static_digest_of)
    .bind(
        reports
            .iter()
            .map(|r| r.report.reporter_count)
            .collect::<Vec<_>>(),
    )
    .bind(reports.iter().map(|r| r.territory_id).collect::<Vec<_>>())
    .bind(
        reports
            .iter()
            .map(|r| r.report.source.as_str())
            .collect::<Vec<_>>(),
    )
    .bind(
        reports
            .iter()
            .map(|r| r.report.visibility)
            .collect::<Vec<_>>(),
    )
    .bind(
        reports
            .iter()
            .map(|r| r.report.idempotency_key.as_deref())
            .collect::<Vec<_>>(),
    )
    .bind(runtime)
    .bind(raw)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_the_typed_form_cannot_reproduce_is_kept_raw() {
        let compactable = serde_json::json!({
            "territory": "Detlas",
            "guild": {"uuid": "u", "name": "Guild", "prefix": "G", "color": [1, 2, 3]},
            "acquired": "2026-07-20T16:48:22.328+00:00",
            "location": {"start": [1, 2], "end": [3, 4]},
            "runtime": {"defense_tier": "Medium", "provenance": {
                "source": "fabric_reporter", "visibility": "public",
                "confidence": 0.550000011920929, "reporter_count": 1,
                "observed_at": "2026-07-20T17:01:10.199510365Z"}},
            "idempotency_key": "k"
        });
        let mut unknown_field = compactable.clone();
        unknown_field["runtime"]["future_field"] = serde_json::json!(true);
        let mut reformatted_time = compactable.clone();
        reformatted_time["acquired"] = serde_json::json!("2026-07-20T16:48:22.328Z");

        assert!(matches!(compact(&compactable), Some(Body::Compact { .. })));
        assert_eq!(compact(&unknown_field), None);
        assert_eq!(compact(&reformatted_time), None);
    }
}
