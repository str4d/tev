use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::RwLock,
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{anyhow, Context};
use fuser::{FileAttr, FileType, Filesystem, MountOption};
use steam_vent_proto::content_manifest::{
    content_manifest_payload::FileMapping, ContentManifestMetadata,
};

use crate::{
    cli::MountBackup,
    formats::{csd::ChunkStore, manifest::Manifest, sis::StockKeepingUnit},
};

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
        let name = filesystem.sku.name.clone();

        fuser::mount2(
            filesystem,
            self.mountpoint,
            &[
                MountOption::RO,
                MountOption::FSName(name),
                MountOption::AutoUnmount,
            ],
        )
        .context("Runtime")?;

        Ok(())
    }
}

fn steam_to_filetype(file_mapping: Option<&FileMapping>) -> FileType {
    if let Some(file_mapping) = file_mapping {
        if file_mapping.flags() & 0b0100_0000 != 0 {
            FileType::Directory
        } else {
            FileType::RegularFile
        }
    } else {
        // Synthetic nodes are always directories.
        FileType::Directory
    }
}

enum Node {
    Real {
        metadata: Rc<ContentManifestMetadata>,
        path: PathBuf,
        file_mapping: FileMapping,
    },
    Synthetic {
        metadata: Rc<ContentManifestMetadata>,
        name: String,
    },
}

impl Node {
    fn metadata(&self) -> &Rc<ContentManifestMetadata> {
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

    /// Returns the size of this file in bytes and "blocks".
    fn size(&self) -> (u64, u64) {
        let bytes = self.file_mapping().map(|f| f.size()).unwrap_or(0);
        let blocks = (bytes + u64::from(BLKSIZE - 1)) / u64::from(BLKSIZE);
        (bytes, blocks)
    }

    fn kind(&self) -> FileType {
        steam_to_filetype(self.file_mapping())
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

    fn attr(&self, ino: u64) -> FileAttr {
        let (size, blocks) = self.size();
        let crtime = UNIX_EPOCH + Duration::new(u64::from(self.metadata().creation_time()), 0);

        FileAttr {
            ino,
            size,
            blocks,
            atime: crtime,
            mtime: crtime,
            ctime: crtime,
            crtime,
            kind: self.kind(),
            perm: 0o0755,
            nlink: 1,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            blksize: BLKSIZE,
            flags: 0,
        }
    }
}

const BLKSIZE: u32 = 512;

const ROOT_ATTR: &FileAttr = &FileAttr {
    ino: 1,
    size: 0,
    blocks: 0,
    atime: UNIX_EPOCH,
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::Directory,
    perm: 0o0755,
    nlink: 1,
    uid: 1000,
    gid: 1000,
    rdev: 0,
    blksize: BLKSIZE,
    flags: 0,
};

struct BackupFs {
    sku: StockKeepingUnit,
    chunks: HashMap<[u8; 20], Rc<RwLock<ChunkStore>>>,
    blocks: u64,
    /// The filesystem's inodes, excluding the root.
    ///
    /// The inode of a node in this vec is `pos + 2`.
    inodes: Vec<Node>,
    /// A map from directory inodes to their contents.
    dir_map: HashMap<u64, Vec<u64>>,
    /// Open files map to inodes because the backup contents can never change.
    open_files: HashMap<u64, u64>,
    open_dirs: HashMap<u64, u64>,
    next_file_fh: u64,
    next_dir_fh: u64,
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
                let manifest = Manifest::read(&manifest_path).with_context(|| {
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

        // Open all of the chunkstores.
        let chunkstores = sku
            .chunkstores
            .iter()
            .flat_map(|(depot, chunkstores)| {
                let base_dir = &base_dir;
                chunkstores.keys().map(move |chunkstore_index| {
                    ChunkStore::open(base_dir, *depot, *chunkstore_index)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut chunks = HashMap::new();
        for chunkstore in chunkstores {
            let chunk_shas = chunkstore
                .csm
                .chunks
                .iter()
                .map(|(sha, _)| *sha)
                .collect::<Vec<_>>();

            let chunkstore = Rc::new(RwLock::new(chunkstore));
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

                let metadata = Rc::new(metadata);

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

        // We can count the number of "blocks" in the filesystem now, because we will only
        // be adding directory inodes after this which have a size of zero.
        let blocks = inodes.iter().map(|node| node.size().1).sum();

        // Generate a map from paths to inodes. We only need this temporarily.
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

        Ok(Self {
            sku,
            chunks,
            blocks,
            inodes,
            dir_map,
            open_files: HashMap::new(),
            open_dirs: HashMap::new(),
            next_file_fh: 0,
            next_dir_fh: 0,
        })
    }

    fn get_node(&self, ino: u64) -> Option<&Node> {
        if let Some(index) = ino.checked_sub(2) {
            self.inodes.get(index as usize)
        } else {
            None
        }
    }
}

const TTL: &Duration = &Duration::from_secs(10);

impl Filesystem for BackupFs {
    fn lookup(
        &mut self,
        _req: &fuser::Request<'_>,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: fuser::ReplyEntry,
    ) {
        if let Some(entries) = self.dir_map.get(&parent) {
            for &ino in entries {
                let node = self.get_node(ino).expect("correct by construction");
                if node.name() == name {
                    reply.entry(TTL, &node.attr(ino), 1);
                    return;
                }
            }
            // Not found.
            reply.error(libc::ENOENT);
        } else {
            reply.error(libc::EINVAL);
        }
    }

    fn getattr(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        fh: Option<u64>,
        reply: fuser::ReplyAttr,
    ) {
        // The filesystem is immutable, so we don't need to separately cache data for
        // potentially-deleted inodes. Instead just verify the file handle.
        if let Some(fh) = fh {
            if let Some(expected_ino) = self.open_dirs.get(&fh) {
                if *expected_ino != ino {
                    reply.error(libc::EBADF);
                    return;
                }
            } else if let Some(expected_ino) = self.open_files.get(&fh) {
                if *expected_ino != ino {
                    reply.error(libc::EBADF);
                    return;
                }
            } else {
                reply.error(libc::EBADF);
                return;
            }
        }

        if ino == 1 {
            reply.attr(TTL, ROOT_ATTR);
        } else if let Some(node) = self.get_node(ino) {
            reply.attr(TTL, &node.attr(ino));
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn open(&mut self, _req: &fuser::Request<'_>, ino: u64, _flags: i32, reply: fuser::ReplyOpen) {
        let fh = self.next_file_fh;
        self.open_files.insert(fh, ino);
        self.next_file_fh = self.next_file_fh.wrapping_add(1);
        reply.opened(fh, 0);
    }

    fn read(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyData,
    ) {
        let offset = offset as u64;

        // The filesystem is immutable, so we don't need to separately cache data for
        // potentially-deleted inodes. Instead just verify the file handle.
        match (self.get_node(ino), self.open_files.get(&fh)) {
            (Some(node), Some(expected_ino)) if *expected_ino == ino => {
                let file_size = node.size().0;

                if offset > file_size {
                    reply.error(libc::EINVAL);
                    return;
                }

                let file_mapping = match node.file_mapping() {
                    Some(f) => f,
                    None => {
                        // Attempted to read a directory as a file.
                        reply.error(libc::EINVAL);
                        return;
                    }
                };

                // If we have nothing to read, no need to access the chunkstores.
                let to_read = u64::min(u64::from(size), file_size - offset);
                if to_read == 0 {
                    reply.data(&[]);
                    return;
                }

                // Prepare the buffer into which we'll read chunks.
                let mut buf = vec![0; to_read as usize];

                // Find the relevant chunks.
                for chunk in &file_mapping.chunks {
                    // Determine how the buffer and chunk overlap.
                    let read_start = offset;
                    let read_end = offset + to_read;
                    let chunk_start = chunk.offset();
                    let chunk_end = chunk.offset() + u64::from(chunk.cb_original());

                    if read_start < chunk_end || chunk_start < read_end {
                        // This chunk contains requested data.
                        let sha = chunk.sha().try_into().unwrap();
                        let chunkstore = self.chunks.get(&sha).expect("correct by construction");
                        let mut chunkstore = chunkstore.write().unwrap();
                        match chunkstore.chunk_data(sha) {
                            Ok(chunk_data) => {
                                let buf = &mut buf[usize::try_from(
                                    chunk_start.saturating_sub(read_start),
                                )
                                .unwrap()..];
                                let chunk_data = &chunk_data[usize::try_from(
                                    read_start.saturating_sub(chunk_start),
                                )
                                .unwrap()..];
                                let chunk_read = usize::min(buf.len(), chunk_data.len());

                                buf[..chunk_read].copy_from_slice(&chunk_data[..chunk_read]);
                            }
                            Err(_) => {
                                reply.error(libc::EIO);
                                return;
                            }
                        };
                    }
                }

                reply.data(&buf);
            }
            _ => reply.error(libc::EBADF),
        }
    }

    fn release(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        if let Some(expected_ino) = self.open_files.remove(&fh) {
            if expected_ino == ino {
                reply.ok();
            } else {
                // Put it back.
                self.open_files.insert(fh, expected_ino);
                reply.error(libc::EBADF);
            }
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn opendir(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        _flags: i32,
        reply: fuser::ReplyOpen,
    ) {
        let fh = self.next_dir_fh;
        self.open_dirs.insert(fh, ino);
        self.next_dir_fh = self.next_dir_fh.wrapping_add(1);
        reply.opened(fh, 0);
    }

    fn readdir(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        let offset = (offset as u64) as usize;

        // The filesystem is immutable, so we don't need to separately cache data for
        // potentially-deleted inodes. Instead just verify the file handle.
        match (self.dir_map.get(&ino), self.open_dirs.get(&fh)) {
            (Some(dir_map), Some(expected_ino)) if *expected_ino == ino => {
                for (entry_offset, &entry_ino) in dir_map.iter().enumerate().skip(offset) {
                    // Apparently this is 1-indexed.
                    let offset = entry_offset as i64 + 1;
                    let node = self.get_node(entry_ino).expect("valid by construction");
                    if reply.add(entry_ino, offset, node.kind(), node.name()) {
                        break;
                    }
                }
                reply.ok();
            }
            _ => reply.error(libc::EBADF),
        }
    }

    fn releasedir(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        _flags: i32,
        reply: fuser::ReplyEmpty,
    ) {
        if let Some(expected_ino) = self.open_dirs.remove(&fh) {
            if expected_ino == ino {
                reply.ok();
            } else {
                // Put it back.
                self.open_dirs.insert(fh, expected_ino);
                reply.error(libc::EBADF);
            }
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn statfs(&mut self, _req: &fuser::Request<'_>, _ino: u64, reply: fuser::ReplyStatfs) {
        reply.statfs(
            self.blocks,
            0,
            0,
            u64::try_from(self.inodes.len()).unwrap() + 1,
            0,
            // Same as the average chunk size.
            1024 * 1024,
            255,
            // Same as the average chunk size.
            1024 * 1024,
        );
    }
}
