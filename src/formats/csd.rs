use std::collections::HashMap;
use std::fs::Metadata;
use std::io::{Cursor, Read, SeekFrom};
use std::path::Path;

use anyhow::anyhow;
use sha1::{Digest, Sha1};
use tokio::io::AsyncReadExt;
use tokio::{
    fs::File,
    io::{AsyncSeekExt, BufReader},
};
use zip::ZipArchive;

use super::csm::ChunkStoreManifest;

pub(crate) struct ChunkStore {
    pub(crate) csm: ChunkStoreManifest,
    csd: BufReader<File>,
    pub(crate) csm_filename: String,
    pub(crate) csd_filename: String,
    pub(crate) csd_metadata: Metadata,
    chunk_map: HashMap<[u8; 20], usize>,
    position: u64,
    buffer: Vec<u8>,
}

impl ChunkStore {
    pub(crate) async fn open(
        base_dir: &Path,
        depot: u32,
        chunkstore_index: u32,
    ) -> anyhow::Result<Self> {
        let csm_filename = format!("{depot}_depotcache_{chunkstore_index}.csm");
        let csm_path = base_dir.join(&csm_filename);
        let csd_path = csm_path.with_extension("csd");
        let csd_filename = csd_path
            .file_name()
            .expect("exists")
            .to_str()
            .expect("valid")
            .into();

        let csm = ChunkStoreManifest::read(&csm_path)?;
        if csm.depot != depot {
            return Err(anyhow!(
                "{} is actually for a different depot {}",
                csm_filename,
                csm.depot,
            ));
        }
        if csm.is_encrypted {
            return Err(anyhow!(
                "{} is encrypted, which should not be the case for backups.",
                csm_filename,
            ));
        }

        let csd = File::open(&csd_path).await?;
        let csd_metadata = csd.metadata().await?;

        let chunk_map = csm
            .chunks
            .iter()
            .enumerate()
            .map(|(i, (sha, _))| (*sha, i))
            .collect();

        Ok(Self {
            csm,
            csd: BufReader::new(csd),
            csm_filename,
            csd_filename,
            csd_metadata,
            chunk_map,
            position: 0,
            buffer: vec![],
        })
    }

    pub(crate) async fn chunk_data(&mut self, sha: [u8; 20]) -> anyhow::Result<Vec<u8>> {
        let (_, chunk) = self
            .csm
            .chunks
            .get(*self.chunk_map.get(&sha).ok_or(anyhow!("Unknown chunk"))?)
            .expect("correct by construction");

        // Read the chunk.
        if chunk.offset != self.position {
            // The chunk is not sequential in the file. Discard the buffer and seek.
            self.csd.seek(SeekFrom::Start(chunk.offset)).await?;
            self.position = chunk.offset;
        }
        self.buffer.resize(chunk.compressed_length.try_into()?, 0);
        self.csd.read_exact(&mut self.buffer).await?;
        self.position += u64::from(chunk.compressed_length);

        // Grab the buffer so we can move it to a blocking thread.
        let compressed = std::mem::take(&mut self.buffer);
        let uncompressed_length = usize::try_from(chunk.uncompressed_length)?;

        match tokio::task::spawn_blocking(move || {
            decompress_and_verify(compressed, uncompressed_length, sha)
        })
        .await??
        {
            Checked::Valid { compressed, data } => {
                // Put the buffer back to reuse for the next chunk.
                let _ = std::mem::replace(&mut self.buffer, compressed);
                Ok(data)
            }
            Checked::WrongLength => Err(anyhow!(
                "Chunk in {} at offset {} does not match uncompressed length in {}",
                self.csd_filename,
                chunk.offset,
                self.csm_filename,
            )),
            Checked::WrongDigest => Err(anyhow!(
                "Chunk in {} at offset {} does not match digest in {}",
                self.csd_filename,
                chunk.offset,
                self.csm_filename,
            )),
        }
    }
}

fn decompress_and_verify(
    compressed: Vec<u8>,
    uncompressed_length: usize,
    sha: [u8; 20],
) -> anyhow::Result<Checked> {
    // Decompress the chunk.
    let mut data = Vec::with_capacity(uncompressed_length);
    let decompressed = match &compressed[..2] {
        b"VZ" => Err(anyhow!("TODO: Implement LZMA decompression")),
        b"PK" => Ok(ZipArchive::new(Cursor::new(&compressed))?
            .by_index(0)?
            .read_to_end(&mut data)?),
        x => Err(anyhow!("Unknown chunk compression type {}", hex::encode(x))),
    }?;
    if decompressed != uncompressed_length {
        return Ok(Checked::WrongLength);
    }

    // Verify the chunk digest.
    let digest = Sha1::digest(&data);
    if digest == sha.into() {
        Ok(Checked::Valid { compressed, data })
    } else {
        Ok(Checked::WrongDigest)
    }
}

enum Checked {
    Valid { compressed: Vec<u8>, data: Vec<u8> },
    WrongLength,
    WrongDigest,
}
