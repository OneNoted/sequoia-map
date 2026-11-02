//! Binary form of the official-API state changes recorded at one stream position: a format
//! byte followed by postcard `(territory id, changed fields)` pairs.

use sequoia_shared::TerritoryStateField;

const FORMAT: u8 = 1;

pub type Changes = Vec<(i16, Vec<TerritoryStateField>)>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StateError {
    #[error("state change format {0} is not supported")]
    Format(u8),
    #[error("state changes do not decode: {0}")]
    Body(String),
}

pub fn encode(changes: &Changes) -> Vec<u8> {
    let mut bytes = vec![FORMAT];
    bytes.extend(postcard::to_allocvec(changes).expect("state changes serialize"));
    bytes
}

/// The stored size of `changes` once their territories have dictionary ids, at most.
pub fn encoded_len(changes: &[(String, Vec<TerritoryStateField>)]) -> usize {
    let widest_id: Changes = changes
        .iter()
        .map(|(_, fields)| (i16::MAX, fields.clone()))
        .collect();
    encode(&widest_id).len()
}

pub fn decode(bytes: &[u8]) -> Result<Changes, StateError> {
    match bytes.split_first() {
        Some((&FORMAT, body)) => {
            postcard::from_bytes(body).map_err(|error| StateError::Body(error.to_string()))
        }
        Some((&format, _)) => Err(StateError::Format(format)),
        None => Err(StateError::Body("empty".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use sequoia_shared::Resources;

    use super::*;

    #[test]
    fn a_field_that_became_unknown_stays_distinct_from_an_unchanged_one() {
        let changes = vec![(
            7,
            vec![
                TerritoryStateField::Treasury(None),
                TerritoryStateField::Stored(Some(Resources {
                    emeralds: 120_000,
                    ..Resources::default()
                })),
            ],
        )];
        assert_eq!(decode(&encode(&changes)), Ok(changes));
        assert_eq!(decode(&[9, 0]), Err(StateError::Format(9)));
    }
}
