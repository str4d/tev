#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use anyhow::anyhow;
use byte_unit::{Byte, UnitType};

use crate::{cli::Inspect, formats};

impl Inspect {
    pub(crate) fn run(&self) -> anyhow::Result<()> {
        match self.path.extension() {
            Some(s) if s.eq_ignore_ascii_case("sis") => {
                let sku = formats::sis::StockKeepingUnit::read(&self.path)?;
                println!("SKU: {} (Disk {}/{})", sku.name, sku.disk, sku.disks);
                println!("Backup: {}", sku.backup);
                println!("Content type: {}", sku.contenttype);
                println!("Apps:");
                for app in sku.apps {
                    println!("- {app}");
                }
                println!("Depots:");
                for depot in sku.depots {
                    print!("- {depot}");
                    if let Some(manifest) = sku.manifests.get(&depot) {
                        print!(", manifest: {manifest}");
                    } else {
                        println!(", missing manifest");
                    }
                    if let Some(chunkstores) = sku.chunkstores.get(&depot) {
                        let size = Byte::from_u64(
                            chunkstores.values().copied().map(u64::from).sum::<u64>(),
                        )
                        .get_appropriate_unit(UnitType::Binary);
                        println!(", Size: {size:#.2}");
                    } else {
                        println!(", missing chunkstores");
                    }
                }
            }
            Some(s) if s.eq_ignore_ascii_case("csm") => {
                let manifest = formats::csm::ChunkStoreManifest::read(&self.path)?;
                println!("ChunkStore manifest");
                println!("Encrypted: {}", manifest.is_encrypted);
                println!("Depot: {}", manifest.depot);
                println!("Chunks: {}", manifest.chunks.len());

                let (compressed_size, uncompressed_size) = manifest
                    .chunks
                    .iter()
                    .map(|(_, chunk)| {
                        (
                            u64::from(chunk.compressed_length),
                            u64::from(chunk.uncompressed_length),
                        )
                    })
                    .fold((0, 0), |(acc_c, acc_u), (c_len, u_len)| {
                        (acc_c + c_len, acc_u + u_len)
                    });

                let compressed_size =
                    Byte::from_u64(compressed_size).get_appropriate_unit(UnitType::Binary);
                println!("Compressed size: {compressed_size:#.2}");

                let uncompressed_size =
                    Byte::from_u64(uncompressed_size).get_appropriate_unit(UnitType::Binary);
                println!("Uncompressed size: {uncompressed_size:#.2}");
            }
            Some(s) if s.eq_ignore_ascii_case("csd") => {
                let filename = self.path.file_stem().expect("present").to_string_lossy();
                let depot = filename
                    .split('_')
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .ok_or(anyhow!("Invalid CSD name"))?;

                let metadata = std::fs::metadata(&self.path)?;

                println!("ChunkStore data");
                println!("Depot: {}", depot);

                let compressed_size =
                    Byte::from_u64(metadata.size()).get_appropriate_unit(UnitType::Binary);
                println!("Compressed size: {compressed_size:#.2}");
            }
            _ => println!("Unknown format"),
        }

        Ok(())
    }
}