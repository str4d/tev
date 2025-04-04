use std::path::Path;

use anyhow::{anyhow, Context};
use futures_util::future;

use crate::{
    cli::VerifyBackup,
    formats::{csd::ChunkStore, manifest::Manifest, sis::StockKeepingUnit},
};

impl VerifyBackup {
    pub(crate) async fn run(self) -> anyhow::Result<()> {
        for path in self.path {
            if let Err(e) = verify_backup(&path, self.manifest_dir.as_deref()).await {
                println!("Failed to verify {}: {e}", path.display());
            }
        }

        Ok(())
    }
}

async fn verify_backup(path: &Path, manifest_dir: Option<&Path>) -> anyhow::Result<()> {
    println!();

    let base_dir = {
        let metadata = path.metadata()?;
        if metadata.is_dir() {
            Ok(path.to_path_buf())
        } else if metadata.is_file() {
            Ok(path
                .parent()
                .expect("Files always have parents")
                .to_path_buf())
        } else {
            Err(anyhow!("Path does not exist"))
        }?
    };

    let sku = StockKeepingUnit::read(&base_dir.join("sku.sis"))?;
    println!("Game: {}", sku.name);

    let mut valid = true;

    for depot in sku.depots {
        println!("Verifying depot {depot}");

        let manifest = manifest_dir
            .zip(sku.manifests.get(&depot))
            .map(|(manifest_dir, manifest_id)| {
                let manifest_path =
                    manifest_dir.join(format!("{}_{}.manifest", depot, manifest_id));
                let manifest = Manifest::open(&manifest_path).with_context(|| {
                    format!(
                        "Cannot find manifest {manifest_id} for depot {depot} in {}",
                        manifest_dir.display()
                    )
                })?;
                if manifest.metadata.depot_id() == depot {
                    if manifest.metadata.filenames_encrypted() {
                        println!(
                            "Manifest {manifest_id} for depot {depot} has encrypted filenames"
                        );
                    }
                    Ok(manifest)
                } else {
                    Err(anyhow!(
                        "{} does not belong to depot {depot}",
                        manifest_path.display()
                    ))
                }
            })
            .transpose()?;

        let chunkstores = sku
            .chunkstores
            .get(&depot)
            .ok_or(anyhow!("Missing chunkstore for depot {depot}"))?;

        let mut depot_chunks = 0;

        for res in future::join_all(chunkstores.iter().map(
            |(&chunkstore_index, &chunkstore_length)| {
                if let Ok(chunkstore_length) = u64::try_from(chunkstore_length) {
                    let base_dir = base_dir.clone();
                    tokio::spawn(async move {
                        verify_chunkstore(
                            &base_dir,
                            depot,
                            chunkstore_index,
                            chunkstore_length,
                        )
                        .await
                    })
                } else {
                    // Chunkstore length is -1; no idea what that means.
                    tokio::spawn(std::future::ready(Some(0)))
                }
            },
        ))
        .await
        {
            if let Some(chunks_read) = res? {
                depot_chunks += chunks_read;
            } else {
                valid = false;
            }
        }

        if let Some(manifest) = manifest {
            let unique_chunks = manifest.metadata.unique_chunks();
            if unique_chunks != depot_chunks {
                println!("Depot {depot} has {unique_chunks} chunks in manifest but {depot_chunks} chunks on disk");
            }
        }
    }

    if valid {
        println!("Depot files match SKU!");
    }

    Ok(())
}

async fn verify_chunkstore(
    base_dir: &Path,
    depot: u32,
    chunkstore_index: u32,
    chunkstore_length: u64,
) -> Option<u32> {
    let mut valid = true;

    let mut chunkstore = match ChunkStore::open(base_dir, depot, chunkstore_index).await {
        Ok(chunkstore) => chunkstore,
        Err(e) => {
            println!("- {e}");
            return None;
        }
    };

    if chunkstore.csd_metadata.len() != chunkstore_length {
        valid = false;
        println!(
            "- {} should be {} bytes according to the SKU, but is actually {} bytes",
            chunkstore.csm_filename,
            chunkstore_length,
            chunkstore.csd_metadata.len(),
        );
    }

    let mut bytes_read = 0;
    let chunks = chunkstore.csm.chunks.clone();
    let num_chunks = chunks.len();

    for (sha, chunk) in chunks {
        if let Err(e) = chunkstore.chunk_data(sha).await {
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

    valid.then_some(num_chunks as u32)
}
