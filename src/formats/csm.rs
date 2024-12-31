use std::path::Path;

use anyhow::anyhow;
use nom::Finish;

#[derive(Debug)]
pub(crate) struct ChunkStoreManifest {
    pub(crate) is_encrypted: bool,
    pub(crate) depot: u32,
    pub(crate) chunks: Vec<([u8; 20], Chunk)>,
}

#[derive(Debug)]
pub(crate) struct Chunk {
    pub(crate) offset: u64,
    pub(crate) uncompressed_length: u32,
    pub(crate) compressed_length: u32,
}

impl ChunkStoreManifest {
    pub(crate) fn read(path: &Path) -> anyhow::Result<Self> {
        if !path
            .extension()
            .map_or(false, |s| s.eq_ignore_ascii_case("csm"))
        {
            return Err(anyhow!(
                "ChunkStoreManifest file does not have extension .csm"
            ));
        }

        let data = std::fs::read(path)?;

        let (_, manifest) = read::manifest(&data)
            .finish()
            .map_err(|e| anyhow!("Failed to parse ChunkStoreManifest: {:?}", e))?;

        Ok(manifest)
    }
}

mod read {
    use nom::{
        branch::alt,
        bytes::complete::{tag, take},
        combinator::{map, value},
        multi::length_count,
        number::complete::{le_u32, le_u64},
        sequence::{preceded, tuple},
        IResult,
    };

    use super::{Chunk, ChunkStoreManifest};

    pub(super) fn manifest(input: &[u8]) -> IResult<&[u8], ChunkStoreManifest> {
        preceded(
            tag("SCFS\x14\x00\x00\x00"),
            map(
                tuple((
                    alt((
                        value(false, tag(b"\x02\x00\x00\x00")),
                        value(true, tag(b"\x03\x00\x00\x00")),
                    )),
                    le_u32,
                    length_count(le_u32, chunk),
                )),
                |(is_encrypted, depot, chunks)| ChunkStoreManifest {
                    is_encrypted,
                    depot,
                    chunks,
                },
            ),
        )(input)
    }

    fn chunk(input: &[u8]) -> IResult<&[u8], ([u8; 20], Chunk)> {
        map(
            tuple((take(20_usize), le_u64, le_u32, le_u32)),
            |(sha, offset, uncompressed_length, compressed_length): (&[u8], _, _, _)| {
                (
                    sha.try_into().expect("correct length"),
                    Chunk {
                        offset,
                        uncompressed_length,
                        compressed_length,
                    },
                )
            },
        )(input)
    }
}
