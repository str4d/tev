use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use anyhow::{anyhow, Context};
use futures_util::future;
use steam_vent::proto::content_manifest::{
    content_manifest_payload::FileMapping, ContentManifestMetadata,
};
use tokio::runtime::{Builder, Runtime};

use crate::{
    cli::MountBackup,
    formats::{csd::ChunkStore, manifest::Manifest, sis::StockKeepingUnit},
};

#[cfg(unix)]
mod fuse;
#[cfg(windows)]
mod windows;

impl MountBackup {
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

        let filesystem = BackupFs::prepare(base_dir, self.manifest_dir)
            .context("Failed to prepare filesystem")?;

        filesystem.mount(self.mountpoint)?;

        Ok(())
    }
}

fn is_dir(file_mapping: Option<&FileMapping>) -> bool {
    if let Some(file_mapping) = file_mapping {
        if file_mapping.flags() & 0b0100_0000 != 0 {
            true
        } else {
            false
        }
    } else {
        // Synthetic nodes are always directories.
        true
    }
}

enum Node {
    Real {
        metadata: Arc<ContentManifestMetadata>,
        path: PathBuf,
        file_mapping: FileMapping,
    },
    Synthetic {
        metadata: Arc<ContentManifestMetadata>,
        name: String,
    },
}

impl Node {
    fn metadata(&self) -> &Arc<ContentManifestMetadata> {
        match self {
            Node::Real { metadata, .. } => metadata,
            Node::Synthetic { metadata, .. } => metadata,
        }
    }

    fn file_mapping(&self) -> Option<&FileMapping> {
        match self {
            Node::Real { file_mapping, .. } => Some(file_mapping),
            Node::Synthetic { .. } => None,
        }
    }

    /// Returns the size of this file in bytes, or 0 for a directory.
    fn size(&self) -> u64 {
        self.file_mapping().map(|f| f.size()).unwrap_or(0)
    }

    fn path(&self) -> Option<&Path> {
        // We only need paths for real nodes.
        match self {
            Node::Real { path, .. } => Some(path),
            Node::Synthetic { .. } => None,
        }
    }

    fn name(&self) -> &str {
        match self {
            Node::Real { path, .. } => path
                .file_name()
                .map(|n| n.to_str().expect("valid str"))
                .unwrap_or(path.to_str().expect("valid str")),
            Node::Synthetic { name, .. } => name,
        }
    }
}

const ROOT_INODE: u64 = 1;

struct BackupFs {
    sku: StockKeepingUnit,
    runtime: Runtime,
    chunks: HashMap<[u8; 20], Arc<RwLock<ChunkStore>>>,
    /// The filesystem's inodes, excluding the root.
    ///
    /// The inode of a node in this vec is `pos + 2`.
    inodes: Vec<Node>,
    /// A map from directory inodes to their contents.
    dir_map: HashMap<u64, Vec<u64>>,
    #[cfg(unix)]
    fuse_info: fuse::FsInfo,
    #[cfg(windows)]
    windows_info: windows::FsInfo,
}

impl BackupFs {
    fn prepare(base_dir: PathBuf, manifest_dir: PathBuf) -> anyhow::Result<Self> {
        let sku = StockKeepingUnit::read(&base_dir.join("sku.sis"))
            .with_context(|| format!("Cannot find sku.sis in {}", base_dir.display()))?;

        // Read all of the manifests into memory.
        let manifests = sku
            .manifests
            .iter()
            .map(|(depot, manifest)| {
                let manifest_path = manifest_dir.join(format!("{}_{}.manifest", depot, manifest));
                let manifest = Manifest::open(&manifest_path).with_context(|| {
                    format!(
                        "Cannot find manifest {manifest} for depot {depot} in {}",
                        manifest_dir.display()
                    )
                })?;
                if manifest.metadata.depot_id() == *depot {
                    Ok(manifest)
                } else {
                    Err(anyhow!(
                        "{} does not belong to depot {depot}",
                        manifest_path.display()
                    ))
                }
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let runtime = Builder::new_current_thread().build()?;

        // Open all of the chunkstores.
        let chunkstores = runtime
            .block_on(future::join_all(sku.chunkstores.iter().flat_map(
                |(depot, chunkstores)| {
                    let base_dir = &base_dir;
                    chunkstores.keys().map(move |chunkstore_index| {
                        ChunkStore::open(base_dir, *depot, *chunkstore_index)
                    })
                },
            )))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;

        let mut chunks = HashMap::new();
        for chunkstore in chunkstores {
            let chunk_shas = chunkstore
                .csm
                .chunks
                .iter()
                .map(|(sha, _)| *sha)
                .collect::<Vec<_>>();

            let chunkstore = Arc::new(RwLock::new(chunkstore));
            for sha in chunk_shas {
                chunks.insert(sha, chunkstore.clone());
            }
        }

        // Assign inodes for each file in the backup.
        let mut inodes = manifests
            .into_iter()
            .flat_map(|manifest| {
                let Manifest {
                    payload, metadata, ..
                } = manifest;

                let metadata = Arc::new(metadata);

                payload.mappings.into_iter().map(move |mut file_mapping| {
                    // Convert file names into platform paths.
                    let filename = file_mapping.take_filename();
                    let path = if filename.contains('/') {
                        filename.split('/').collect()
                    } else {
                        filename.split('\\').collect()
                    };

                    Node::Real {
                        metadata: metadata.clone(),
                        path,
                        file_mapping,
                    }
                })
            })
            .collect::<Vec<_>>();

        // Remove any duplicate directories (which can occur across multiple depots).
        inodes.sort_by_key(|node| node.path().expect("all real nodes").to_path_buf());
        inodes.dedup_by(|a, b| a.path() == b.path());

        // Generate a map from paths to inodes.
        let mut path_map = inodes
            .iter()
            .zip(0u64..)
            .map(|(node, index)| {
                let path = node
                    .path()
                    .expect("inodes currently only contains real nodes");
                (path.to_path_buf(), index + 2)
            })
            .collect::<HashMap<_, _>>();
        // Add the root inode to the map.
        path_map.insert(PathBuf::new(), 1);

        // Precompute a directory map from parents to children, adding synthetic inodes as
        // necessary.
        let mut dir_map = HashMap::<_, Vec<_>>::new();
        for index in 0..inodes.len() {
            let node = inodes.get(index).expect("present by construction");
            let metadata = node.metadata().clone();

            let mut ino = (index as u64) + 2;
            let mut parent_path = node
                .path()
                .expect("real by construction")
                .parent()
                .expect("not a root by construction")
                .to_path_buf();

            loop {
                match path_map.get(&parent_path) {
                    Some(parent_ino) => {
                        dir_map.entry(*parent_ino).or_default().push(ino);
                        break;
                    }
                    None => {
                        let parent_ino = (inodes.len() as u64) + 2;
                        let name = parent_path
                            .file_name()
                            .expect("not empty")
                            .to_string_lossy()
                            .into_owned();

                        // We're creating a new node as a parent, so we need to loop and
                        // find its grandparent.
                        let mut path = parent_path
                            .parent()
                            .expect("not root by construction")
                            .to_path_buf();
                        std::mem::swap(&mut parent_path, &mut path);

                        path_map.insert(path, parent_ino);
                        inodes.push(Node::Synthetic {
                            metadata: metadata.clone(),
                            name,
                        });
                        dir_map.entry(parent_ino).or_default().push(ino);

                        ino = parent_ino;
                    }
                }
            }
        }

        #[cfg(unix)]
        let fuse_info = fuse::FsInfo::prepare(&inodes);

        #[cfg(windows)]
        let windows_info = windows::FsInfo::prepare(path_map);

        Ok(Self {
            sku,
            runtime,
            chunks,
            inodes,
            dir_map,
            #[cfg(unix)]
            fuse_info,
            #[cfg(windows)]
            windows_info,
        })
    }
}

fn get_node(inodes: &[Node], ino: u64) -> Option<&Node> {
    if let Some(index) = ino.checked_sub(ROOT_INODE + 1) {
        inodes.get(index as usize)
    } else {
        None
    }
}

fn read_data(
    runtime: &Runtime,
    chunks: &HashMap<[u8; 20], Arc<RwLock<ChunkStore>>>,
    node: &Node,
    offset: u64,
    buf: &mut [u8],
) -> Result<u64, ReadError> {
    let file_size = node.size();

    if offset > file_size {
        return Err(ReadError::InvalidParameter);
    }

    let file_mapping = match node.file_mapping() {
        Some(f) => f,
        None => {
            // Attempted to read a directory as a file.
            return Err(ReadError::InvalidParameter);
        }
    };

    // If we have nothing to read, no need to access the chunkstores.
    let to_read = u64::min(buf.len() as u64, file_size - offset);
    if to_read == 0 {
        return Ok(0);
    }

    // Find the relevant chunks.
    for chunk in &file_mapping.chunks {
        // Determine how the buffer and chunk overlap.
        let read_start = offset;
        let read_end = offset + to_read;
        let chunk_start = chunk.offset();
        let chunk_end = chunk.offset() + u64::from(chunk.cb_original());

        if read_start < chunk_end && chunk_start < read_end {
            // This chunk contains requested data.
            let sha = chunk.sha().try_into().unwrap();
            let chunkstore = chunks.get(&sha).expect("correct by construction");
            let mut chunkstore = chunkstore.write().unwrap();
            match runtime.block_on(chunkstore.chunk_data(sha)) {
                Ok(chunk_data) => {
                    let buf = &mut buf
                        [usize::try_from(chunk_start.saturating_sub(read_start)).unwrap()..];
                    let chunk_data = &chunk_data
                        [usize::try_from(read_start.saturating_sub(chunk_start)).unwrap()..];
                    let chunk_read = usize::min(buf.len(), chunk_data.len());

                    buf[..chunk_read].copy_from_slice(&chunk_data[..chunk_read]);
                }
                Err(_) => {
                    return Err(ReadError::Io);
                }
            };
        }
    }

    Ok(to_read)
}

enum ReadError {
    InvalidParameter,
    Io,
}
