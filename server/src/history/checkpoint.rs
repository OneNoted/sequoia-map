//! Binary form of a complete checkpoint. Ownership is a format byte followed by fixed 14-byte
//! little-endian records of territory id, guild id and acquisition time in microseconds. The
//! official-API states of those territories are kept apart (`history_checkpoint.states`), so
//! ownership bytes stay readable by servers that predate states: a format byte followed by
//! postcard [`StateRecord`]s.

use sequoia_shared::TerritoryState;
use thiserror::Error;

const FORMAT: u8 = 1;
const STATES_FORMAT: u8 = 1;
const RECORD_BYTES: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Holding {
    pub territory_id: i16,
    pub guild_id: i32,
    pub acquired_micros: i64,
}

/// A territory's official-API state and when the API supplied its latest change.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StateRecord {
    pub territory_id: i16,
    pub observed_micros: i64,
    pub state: TerritoryState,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CheckpointError {
    #[error("checkpoint format {0} is not supported")]
    Format(u8),
    #[error("checkpoint body of {0} bytes is not a whole number of records")]
    Truncated(usize),
    #[error("checkpoint is empty")]
    Empty,
    #[error("checkpoint states do not decode: {0}")]
    States(String),
}

pub fn encode(holdings: &[Holding]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(1 + holdings.len() * RECORD_BYTES);
    bytes.push(FORMAT);
    for holding in holdings {
        bytes.extend_from_slice(&holding.territory_id.to_le_bytes());
        bytes.extend_from_slice(&holding.guild_id.to_le_bytes());
        bytes.extend_from_slice(&holding.acquired_micros.to_le_bytes());
    }
    bytes
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Holding>, CheckpointError> {
    let (&format, body) = bytes.split_first().ok_or(CheckpointError::Empty)?;
    if format != FORMAT {
        return Err(CheckpointError::Format(format));
    }
    if body.len() % RECORD_BYTES != 0 {
        return Err(CheckpointError::Truncated(body.len()));
    }
    Ok(body
        .chunks_exact(RECORD_BYTES)
        .map(|record| Holding {
            territory_id: i16::from_le_bytes([record[0], record[1]]),
            guild_id: i32::from_le_bytes([record[2], record[3], record[4], record[5]]),
            acquired_micros: i64::from_le_bytes(
                record[6..14]
                    .try_into()
                    .expect("record slice is eight bytes"),
            ),
        })
        .collect())
}

/// `None` when there are no states to keep.
pub fn encode_states(states: &[StateRecord]) -> Option<Vec<u8>> {
    if states.is_empty() {
        return None;
    }
    let mut bytes = vec![STATES_FORMAT];
    bytes.extend(postcard::to_allocvec(states).expect("states serialize"));
    Some(bytes)
}

pub fn decode_states(bytes: &[u8]) -> Result<Vec<StateRecord>, CheckpointError> {
    let (&format, body) = bytes.split_first().ok_or(CheckpointError::Empty)?;
    if format != STATES_FORMAT {
        return Err(CheckpointError::Format(format));
    }
    postcard::from_bytes(body).map_err(|error| CheckpointError::States(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_rejects_a_torn_record_and_an_unknown_format() {
        let holdings = [Holding {
            territory_id: -3,
            guild_id: 70_000,
            acquired_micros: -1_234_567,
        }];
        let bytes = encode(&holdings);
        assert_eq!(decode(&bytes), Ok(holdings.to_vec()));
        assert_eq!(
            decode(&bytes[..bytes.len() - 1]),
            Err(CheckpointError::Truncated(13))
        );
        let mut future = bytes.clone();
        future[0] = 2;
        assert_eq!(decode(&future), Err(CheckpointError::Format(2)));
    }

    #[test]
    fn states_round_trip_and_a_torn_body_is_rejected() {
        let states = vec![StateRecord {
            territory_id: -3,
            observed_micros: 1_793_618_198_000_000,
            state: TerritoryState {
                headquarters: Some(true),
                guild_headquarters: Some("Orc Road".into()),
                defences: Some("LOW".into()),
                ..TerritoryState::default()
            },
        }];
        assert_eq!(encode_states(&[]), None);
        let bytes = encode_states(&states).expect("states");
        assert_eq!(decode_states(&bytes), Ok(states));
        assert!(matches!(
            decode_states(&bytes[..bytes.len() - 1]),
            Err(CheckpointError::States(_))
        ));
    }
}
