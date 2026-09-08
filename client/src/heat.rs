use chrono::SecondsFormat;
use sequoia_shared::history::{HistoryHeat, HistoryHeatMeta, HistoryHeatSource};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeatFetchInput {
    pub source: HistoryHeatSource,
    pub season_id: Option<i32>,
    pub at: Option<i64>,
}

pub fn build_heat_query(input: HeatFetchInput) -> Result<String, String> {
    let mut params = vec![format!(
        "source={}",
        match input.source {
            HistoryHeatSource::Season => "season",
            HistoryHeatSource::AllTime => "all_time",
        }
    )];

    if matches!(input.source, HistoryHeatSource::Season)
        && let Some(season_id) = input.season_id
    {
        params.push(format!("season_id={season_id}"));
    }

    if let Some(at) = input.at {
        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(at, 0)
            .ok_or_else(|| format!("invalid timestamp: {at}"))?;
        let encoded = dt.to_rfc3339_opts(SecondsFormat::Secs, true);
        params.push(format!("at={encoded}"));
    }

    Ok(params.join("&"))
}

pub async fn fetch_heat_meta() -> Result<HistoryHeatMeta, String> {
    let resp = gloo_net::http::Request::get("/api/history/heat/meta")
        .send()
        .await
        .map_err(|e| format!("fetch error: {e}"))?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<HistoryHeatMeta>()
        .await
        .map_err(|e| format!("parse error: {e}"))
}

pub async fn fetch_heat(input: HeatFetchInput) -> Result<HistoryHeat, String> {
    let query = build_heat_query(input)?;
    let url = format!("/api/history/heat?{query}");
    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch error: {e}"))?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<HistoryHeat>()
        .await
        .map_err(|e| format!("parse error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_heat_query_for_season_total() {
        let query = build_heat_query(HeatFetchInput {
            source: HistoryHeatSource::Season,
            season_id: Some(29),
            at: None,
        })
        .expect("query should build");
        assert_eq!(query, "source=season&season_id=29");
    }

    #[test]
    fn build_heat_query_for_all_time_cumulative() {
        let query = build_heat_query(HeatFetchInput {
            source: HistoryHeatSource::AllTime,
            season_id: None,
            at: Some(1_700_000_000),
        })
        .expect("query should build");
        assert!(query.starts_with("source=all_time&at="));
    }
}
