use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use dokan::{
    CreateFileInfo, DiskSpaceInfo, FileInfo, FileSystemHandler, FileSystemMounter, FillDataError,
    FillDataResult, FindData, MountFlags, MountOptions, OperationInfo, OperationResult, VolumeInfo,
    IO_SECURITY_CONTEXT,
};
use dokan_sys::win32;
use steam_vent::proto::content_manifest::content_manifest_payload::FileMapping;
use widestring::{U16CStr, U16CString};
use winapi::{shared::ntstatus, um::winnt};

use super::{get_node, is_dir, read_data, BackupFs, Node, ReadError, ROOT_INODE};

fn steam_to_attributes(file_mapping: Option<&FileMapping>) -> u32 {
    if is_dir(file_mapping) {
        winnt::FILE_ATTRIBUTE_DIRECTORY
    } else {
        winnt::FILE_ATTRIBUTE_READONLY
    }
}

impl Node {
    fn attributes(&self) -> u32 {
        steam_to_attributes(self.file_mapping())
    }

    fn file_info(&self, ino: u64) -> FileInfo {
        let crtime = UNIX_EPOCH + Duration::new(u64::from(self.metadata().creation_time()), 0);

        FileInfo {
            attributes: self.attributes(),
            creation_time: crtime,
            last_access_time: crtime,
            last_write_time: crtime,
            file_size: self.size(),
            number_of_links: 0,
            file_index: ino,
        }
    }
}

const ROOT_FILE_INFO: FileInfo = FileInfo {
    attributes: winnt::FILE_ATTRIBUTE_DIRECTORY,
    creation_time: UNIX_EPOCH,
    last_access_time: UNIX_EPOCH,
    last_write_time: UNIX_EPOCH,
    file_size: 0,
    number_of_links: 0,
    file_index: 1,
};

pub(super) struct FsInfo {
    path_map: HashMap<U16CString, u64>,
}

impl FsInfo {
    pub(super) fn prepare(path_map: HashMap<PathBuf, u64>) -> Self {
        // Rewrite the path map to the type `dokan` uses.
        let path_map = path_map
            .into_iter()
            .map(|(path, ino)| {
                // `path_map` is provided with no root; add one here.
                let path = format!("\\{}", path.display());
                (
                    U16CString::from_str(&path).expect("valid by construction"),
                    ino,
                )
            })
            .collect();

        Self { path_map }
    }
}

impl BackupFs {
    pub(super) fn mount(self, mountpoint: PathBuf) -> anyhow::Result<()> {
        let mount_point = U16CString::from_os_str(mountpoint.as_os_str())?;

        let (tx, rx) = mpsc::channel();
        ctrlc::set_handler(move || tx.send(()).expect("Could not send signal on channel."))
            .context("Error setting Ctrl-C handler")?;

        let name = self.sku.name.clone();

        let options = MountOptions {
            flags: MountFlags::WRITE_PROTECT,
            ..Default::default()
        };

        dokan::init();
        if dokan::get_driver_version() == 0 {
            return Err(anyhow!("Dokan driver is not installed. Install DokanSetup.exe from https://github.com/dokan-dev/dokany/releases/latest"));
        }

        // Mount the filesystem.
        let mut mounter = FileSystemMounter::new(&self, &mount_point, &options);
        let fs = mounter.mount()?;

        println!("Mounted '{name}' at {}", mountpoint.display());
        println!("Waiting for Ctrl-C...");
        rx.recv().expect("Could not receive from channel");

        // Unmount the filesystem.
        if !dokan::unmount(&mount_point) {
            println!("Failed to unmount the system; program might hang.");
        }
        drop(fs);

        dokan::shutdown();

        Ok(())
    }
}

#[derive(Debug)]
pub struct EntryHandle {
    ino: u64,
}

impl<'c, 'h: 'c> FileSystemHandler<'c, 'h> for BackupFs {
    type Context = EntryHandle;

    fn create_file(
        &'h self,
        file_name: &U16CStr,
        _security_context: &IO_SECURITY_CONTEXT,
        desired_access: winnt::ACCESS_MASK,
        _file_attributes: u32,
        _share_access: u32,
        create_disposition: u32,
        create_options: u32,
        _info: &mut OperationInfo<'c, 'h, Self>,
    ) -> OperationResult<CreateFileInfo<Self::Context>> {
        if desired_access
            & (winnt::GENERIC_WRITE
                | winnt::FILE_WRITE_DATA
                | winnt::FILE_WRITE_ATTRIBUTES
                | winnt::FILE_WRITE_EA
                | winnt::FILE_APPEND_DATA)
            > 0
        {
            // Reject all write attempts.
            Err(ntstatus::STATUS_MEDIA_WRITE_PROTECTED)
        } else if create_disposition == win32::FILE_OPEN {
            match self.windows_info.path_map.get(file_name) {
                // Path does not exist.
                None => Err(ntstatus::STATUS_OBJECT_NAME_NOT_FOUND),
                // Path exists, get its details.
                Some(&ino) => {
                    let is_dir = if ino == ROOT_INODE {
                        true
                    } else {
                        let node = get_node(&self.inodes, ino).expect("correct by construction");
                        is_dir(node.file_mapping())
                    };

                    if (create_options & win32::FILE_DIRECTORY_FILE > 0) && !is_dir {
                        Err(ntstatus::STATUS_NOT_A_DIRECTORY)
                    } else if (create_options & win32::FILE_NON_DIRECTORY_FILE > 0) && is_dir {
                        Err(ntstatus::STATUS_FILE_IS_A_DIRECTORY)
                    } else {
                        Ok(CreateFileInfo {
                            context: EntryHandle { ino },
                            is_dir,
                            new_file_created: false,
                        })
                    }
                }
            }
        } else {
            Err(ntstatus::STATUS_INVALID_PARAMETER)
        }
    }

    fn read_file(
        &'h self,
        _file_name: &U16CStr,
        offset: i64,
        buffer: &mut [u8],
        _info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context,
    ) -> OperationResult<u32> {
        let node = get_node(&self.inodes, context.ino).ok_or(ntstatus::STATUS_INVALID_PARAMETER)?;

        match read_data(&self.runtime, &self.chunks, node, offset as u64, buffer) {
            Ok(read) => Ok(read as u32),
            Err(ReadError::InvalidParameter) => Err(ntstatus::STATUS_INVALID_PARAMETER),
            Err(ReadError::Io) => Err(ntstatus::STATUS_DATA_ERROR),
        }
    }

    fn get_file_information(
        &'h self,
        _file_name: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context,
    ) -> OperationResult<FileInfo> {
        if context.ino == ROOT_INODE {
            Ok(ROOT_FILE_INFO)
        } else {
            let node =
                get_node(&self.inodes, context.ino).ok_or(ntstatus::STATUS_INVALID_PARAMETER)?;
            Ok(node.file_info(context.ino))
        }
    }

    fn find_files(
        &'h self,
        _file_name: &U16CStr,
        mut fill_find_data: impl FnMut(&FindData) -> FillDataResult,
        _info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context,
    ) -> OperationResult<()> {
        match self.dir_map.get(&context.ino) {
            Some(dir_map) => {
                for entry_ino in dir_map {
                    let node = get_node(&self.inodes, *entry_ino).expect("valid by construction");
                    let file_info = node.file_info(*entry_ino);
                    fill_find_data(&FindData {
                        attributes: file_info.attributes,
                        creation_time: file_info.creation_time,
                        last_access_time: file_info.last_access_time,
                        last_write_time: file_info.last_write_time,
                        file_size: file_info.file_size,
                        file_name: U16CString::from_str(node.name()).unwrap(),
                    })
                    .map_err(|e| <FillDataError as Into<i32>>::into(e))?;
                }
                Ok(())
            }
            None => Err(ntstatus::STATUS_INVALID_PARAMETER),
        }
    }

    fn get_disk_free_space(
        &'h self,
        _info: &OperationInfo<'c, 'h, Self>,
    ) -> OperationResult<DiskSpaceInfo> {
        Ok(DiskSpaceInfo {
            byte_count: 0,
            free_byte_count: 0,
            available_byte_count: 0,
        })
    }

    fn get_volume_information(
        &'h self,
        _info: &OperationInfo<'c, 'h, Self>,
    ) -> OperationResult<VolumeInfo> {
        Ok(VolumeInfo {
            name: U16CString::from_str(&self.sku.name).unwrap(),
            serial_number: 0,
            max_component_length: 255,
            fs_flags: 0,
            fs_name: U16CString::from_str("NTFS").unwrap(),
        })
    }
}
