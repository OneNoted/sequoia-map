//! Binary form of a complete ownership checkpoint: a format byte followed by fixed 14-byte
//! little-endian records of territory id, guild id and acquisition time in microseconds.

use thiserror::Error;

const FORMAT: u8 = 1;
const RECORD_BYTES: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Holding {
    pub territory_id: i16,
    pub guild_id: i32,
    pub acquired_micros: i64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CheckpointError {
    #[error("checkpoint format {0} is not supported")]
    Format(u8),
    #[error("checkpoint body of {0} bytes is not a whole number of records")]
    Truncated(usize),
    #[error("checkpoint is empty")]
    Empty,
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
}
