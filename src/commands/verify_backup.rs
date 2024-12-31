use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use anyhow::anyhow;
use sha1::{Digest, Sha1};
use zip::ZipArchive;

use crate::{cli::VerifyBackup, formats};

impl VerifyBackup {
    pub(crate) fn run(self) -> anyhow::Result<()> {
        let base_dir = {
            let metadata = self.path.metadata()?;
            if metadata.is_dir() {
                Ok(self.path)
            } else if metadata.is_file() {
                Ok(self
                    .path
                    .parent()
                    .expect("Files always have parents")
                    .to_path_buf())
            } else {
                Err(anyhow!("Path does not exist"))
            }?
        };

        let sku = formats::sis::StockKeepingUnit::read(&base_dir.join("sku.sis"))?;
        println!("Game: {}", sku.name);

        let mut valid = true;

        for depot in sku.depots {
            println!("Verifying depot {depot}");

            let chunkstores = sku
                .chunkstores
                .get(&depot)
                .ok_or(anyhow!("Missing chunkstore for depot {depot}"))?;

            for (chunkstore_index, chunkstore_length) in chunkstores {
                valid &= verify_chunkstore(
                    &base_dir,
                    depot,
                    *chunkstore_index,
                    u64::from(*chunkstore_length),
                )?;
            }
        }

        if valid {
            println!("Depot files match SKU!");
        }

        Ok(())
    }
}

fn verify_chunkstore(
    base_dir: &Path,
    depot: u32,
    chunkstore_index: u32,
    chunkstore_length: u64,
) -> anyhow::Result<bool> {
    let mut valid = true;

    let csm_filename = format!("{depot}_depotcache_{chunkstore_index}.csm");
    let csm_path = base_dir.join(&csm_filename);
    let csd_path = csm_path.with_extension("csd");
    let csd_filename = csd_path
        .file_name()
        .expect("exists")
        .to_str()
        .expect("valid");

    let csm = formats::csm::ChunkStoreManifest::read(&csm_path)?;
    if csm.depot != depot {
        valid = false;
        println!(
            "- {} is actually for a different depot {}",
            csm_filename, csm.depot,
        );
    }
    if csm.is_encrypted {
        println!(
            "- {} is encrypted, which should not be the case for backups. Cannot verify.",
            csm_filename,
        );
        return Ok(false);
    }

    let csd = File::open(&csd_path)?;
    let csd_metadata = csd.metadata()?;
    if csd_metadata.size() != chunkstore_length {
        valid = false;
        println!(
            "- {} should be {} bytes according to the SKU, but is actually {} bytes",
            csm_filename,
            chunkstore_length,
            csd_metadata.size(),
        );
    }

    let mut csd = BufReader::new(csd);
    let mut position = 0;
    let mut bytes_read = 0;
    let mut chunk_data = vec![];
    let mut decompressed_data = vec![];

    for (sha, chunk) in csm.chunks {
        // Read the chunk.
        if chunk.offset != position {
            // The chunk is not sequential in the file. Discard the buffer and seek.
            csd.seek(std::io::SeekFrom::Start(chunk.offset))?;
            position = chunk.offset;
        }
        chunk_data.resize(chunk.compressed_length.try_into()?, 0);
        csd.read_exact(&mut chunk_data)?;
        position += u64::from(chunk.compressed_length);
        bytes_read += u64::from(chunk.compressed_length);

        // Decompress the chunk.
        decompressed_data.clear();
        let decompressed = match &chunk_data[..2] {
            b"VZ" => Err(anyhow!("TODO: Implement LZMA decompression")),
            b"PK" => Ok(ZipArchive::new(Cursor::new(&chunk_data))?
                .by_index(0)?
                .read_to_end(&mut decompressed_data)?),
            x => Err(anyhow!("Unknown chunk compression type {}", hex::encode(x))),
        }?;
        if decompressed != usize::try_from(chunk.uncompressed_length)? {
            valid = false;
            println!(
                "- Chunk in {} at offset {} does not match uncompressed length in {}",
                csd_filename, chunk.offset, csm_filename,
            );
        }

        // Verify the chunk digest.
        let digest = Sha1::digest(&decompressed_data);
        if digest != sha.into() {
            valid = false;
            println!(
                "- Chunk in {} at offset {} does not match digest in {}",
                csd_filename, chunk.offset, csm_filename,
            );
        }
    }

    if bytes_read != chunkstore_length {
        println!(
            "- {} contains {} bytes that do not correspond to chunks in {}",
            csd_filename,
            chunkstore_length
                .checked_sub(bytes_read)
                .ok_or(anyhow!("{} was read duplicatively", csd_filename))?,
            csm_filename,
        );
    }

    Ok(valid)
}
