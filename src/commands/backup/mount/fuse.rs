use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use fuser::{FileAttr, FileType, Filesystem, MountOption};
use steam_vent_proto::content_manifest::content_manifest_payload::FileMapping;

use super::{get_node, is_dir, read_data, BackupFs, Node, ReadError, ROOT_INODE};

const TTL: &Duration = &Duration::from_secs(10);

fn steam_to_filetype(file_mapping: Option<&FileMapping>) -> FileType {
    if is_dir(file_mapping) {
        FileType::Directory
    } else {
        FileType::RegularFile
    }
}

impl Node {
    /// Returns the size of this file in bytes and "blocks".
    fn blocks(&self) -> u64 {
        (self.size() + u64::from(BLKSIZE - 1)) / u64::from(BLKSIZE)
    }

    fn kind(&self) -> FileType {
        steam_to_filetype(self.file_mapping())
    }

    fn attr(&self, ino: u64) -> FileAttr {
        let crtime = UNIX_EPOCH + Duration::new(u64::from(self.metadata().creation_time()), 0);

        FileAttr {
            ino,
            size: self.size(),
            blocks: self.blocks(),
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
    ino: ROOT_INODE,
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

pub(super) struct FsInfo {
    blocks: u64,
    /// Open files map to inodes because the backup contents can never change.
    open_files: HashMap<u64, u64>,
    open_dirs: HashMap<u64, u64>,
    next_file_fh: u64,
    next_dir_fh: u64,
    read_buf: Vec<u8>,
}

impl FsInfo {
    pub(super) fn prepare(inodes: &[Node]) -> Self {
        let blocks = inodes.iter().map(|node| node.blocks()).sum();
        Self {
            blocks,
            open_files: HashMap::new(),
            open_dirs: HashMap::new(),
            next_file_fh: 0,
            next_dir_fh: 0,
            read_buf: Vec::with_capacity(64 * 1024),
        }
    }
}

impl BackupFs {
    pub(super) fn mount(self, mountpoint: PathBuf) -> anyhow::Result<()> {
        let name = self.sku.name.clone();

        fuser::mount2(
            self,
            mountpoint,
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
                let node = get_node(&self.inodes, ino).expect("correct by construction");
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
            if let Some(expected_ino) = self.fuse_info.open_dirs.get(&fh) {
                if *expected_ino != ino {
                    reply.error(libc::EBADF);
                    return;
                }
            } else if let Some(expected_ino) = self.fuse_info.open_files.get(&fh) {
                if *expected_ino != ino {
                    reply.error(libc::EBADF);
                    return;
                }
            } else {
                reply.error(libc::EBADF);
                return;
            }
        }

        if ino == ROOT_INODE {
            reply.attr(TTL, ROOT_ATTR);
        } else if let Some(node) = get_node(&self.inodes, ino) {
            reply.attr(TTL, &node.attr(ino));
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn open(&mut self, _req: &fuser::Request<'_>, ino: u64, _flags: i32, reply: fuser::ReplyOpen) {
        let fh = self.fuse_info.next_file_fh;
        self.fuse_info.open_files.insert(fh, ino);
        self.fuse_info.next_file_fh = self.fuse_info.next_file_fh.wrapping_add(1);
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
        match (
            get_node(&self.inodes, ino),
            self.fuse_info.open_files.get(&fh),
        ) {
            (Some(node), Some(expected_ino)) if *expected_ino == ino => {
                // Prepare the buffer into which we'll read chunks.
                self.fuse_info.read_buf.resize(size as usize, 0);
                match read_data(
                    &self.runtime,
                    &self.chunks,
                    node,
                    offset,
                    &mut self.fuse_info.read_buf,
                ) {
                    Ok(read) => reply.data(&self.fuse_info.read_buf[..read as usize]),
                    Err(ReadError::InvalidParameter) => reply.error(libc::EINVAL),
                    Err(ReadError::Io) => reply.error(libc::EIO),
                }
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
        if let Some(expected_ino) = self.fuse_info.open_files.remove(&fh) {
            if expected_ino == ino {
                reply.ok();
            } else {
                // Put it back.
                self.fuse_info.open_files.insert(fh, expected_ino);
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
        let fh = self.fuse_info.next_dir_fh;
        self.fuse_info.open_dirs.insert(fh, ino);
        self.fuse_info.next_dir_fh = self.fuse_info.next_dir_fh.wrapping_add(1);
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
        match (self.dir_map.get(&ino), self.fuse_info.open_dirs.get(&fh)) {
            (Some(dir_map), Some(expected_ino)) if *expected_ino == ino => {
                for (entry_offset, &entry_ino) in dir_map.iter().enumerate().skip(offset) {
                    // Apparently this is 1-indexed.
                    let offset = entry_offset as i64 + 1;
                    let node = get_node(&self.inodes, entry_ino).expect("valid by construction");
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
        if let Some(expected_ino) = self.fuse_info.open_dirs.remove(&fh) {
            if expected_ino == ino {
                reply.ok();
            } else {
                // Put it back.
                self.fuse_info.open_dirs.insert(fh, expected_ino);
                reply.error(libc::EBADF);
            }
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn statfs(&mut self, _req: &fuser::Request<'_>, _ino: u64, reply: fuser::ReplyStatfs) {
        reply.statfs(
            self.fuse_info.blocks,
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
