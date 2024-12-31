use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use anyhow::anyhow;

use crate::formats::csd::ChunkStore;
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
                );
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
) -> bool {
    let mut valid = true;

    let mut chunkstore = match ChunkStore::open(base_dir, depot, chunkstore_index) {
        Ok(chunkstore) => chunkstore,
        Err(e) => {
            println!("- {e}");
            return false;
        }
    };

    if chunkstore.csd_metadata.size() != chunkstore_length {
        valid = false;
        println!(
            "- {} should be {} bytes according to the SKU, but is actually {} bytes",
            chunkstore.csm_filename,
            chunkstore_length,
            chunkstore.csd_metadata.size(),
        );
    }

    let mut bytes_read = 0;
    let chunks = chunkstore.csm.chunks.clone();

    for (sha, chunk) in chunks {
        if let Err(e) = chunkstore.chunk_data(sha) {
            valid = false;
            println!("- {e}");
        };
        bytes_read += u64::from(chunk.compressed_length);
    }

    if bytes_read != chunkstore_length {
        match chunkstore_length.checked_sub(bytes_read) {
            Some(excess) => println!(
                "- {} contains {} bytes that do not correspond to chunks in {}",
                chunkstore.csd_filename, excess, chunkstore.csm_filename,
            ),
            None => println!("- {} was read duplicatively", chunkstore.csd_filename),
        }
    }

    valid
}
