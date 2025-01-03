use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use dokan::{
    CreateFileInfo, DiskSpaceInfo, FileInfo, FileSystemHandler, FileSystemMounter, FillDataError,
    FillDataResult, FindData, MountFlags, MountOptions, OperationInfo, OperationResult, VolumeInfo,
    IO_SECURITY_CONTEXT,
};
use steam_vent_proto::content_manifest::content_manifest_payload::FileMapping;
use widestring::{U16CStr, U16CString};
use winapi::{
    shared::ntstatus::{STATUS_DATA_ERROR, STATUS_INVALID_PARAMETER},
    um::winnt,
};

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

impl BackupFs {
    pub(super) fn mount(self, mountpoint: PathBuf) -> anyhow::Result<()> {
        let mount_point = U16CString::from_os_str(mountpoint.as_os_str())?;
        dbg!(mount_point.to_string_lossy());

        let options = MountOptions {
            single_thread: true,
            flags: MountFlags::ALT_STREAM
                | MountFlags::WRITE_PROTECT
                | MountFlags::DEBUG
                | MountFlags::STDERR,
            ..Default::default()
        };

        println!("1");
        let mut mounter = FileSystemMounter::new(&self, &mount_point, &options);
        println!("2");
        let fs = mounter.mount().inspect_err(|e| {
            dbg!(e);
        })?;
        println!("3");
        // Wait for the filesystem to unmount.
        drop(fs);
        println!("4");

        Ok(())
    }
}

pub struct EntryHandle {
    ino: u64,
}

impl<'c, 'h: 'c> FileSystemHandler<'c, 'h> for BackupFs {
    type Context = EntryHandle;

    fn create_file(
        &'h self,
        file_name: &U16CStr,
        security_context: &IO_SECURITY_CONTEXT,
        desired_access: winnt::ACCESS_MASK,
        file_attributes: u32,
        share_access: u32,
        create_disposition: u32,
        create_options: u32,
        info: &mut OperationInfo<'c, 'h, Self>,
    ) -> OperationResult<CreateFileInfo<Self::Context>> {
        println!("create_file called");
        dbg!(file_name.to_string_lossy());
        let ino = 1;

        let is_dir = if ino == ROOT_INODE {
            true
        } else {
            let node = get_node(&self.inodes, ino).expect("correct by construction");
            is_dir(node.file_mapping())
        };

        Ok(CreateFileInfo {
            context: EntryHandle { ino },
            is_dir,
            new_file_created: false,
        })
    }

    fn read_file(
        &'h self,
        _file_name: &U16CStr,
        offset: i64,
        buffer: &mut [u8],
        _info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context,
    ) -> OperationResult<u32> {
        println!("read_file called");
        let node = get_node(&self.inodes, context.ino).ok_or(STATUS_INVALID_PARAMETER)?;

        match read_data(&self.runtime, &self.chunks, node, offset as u64, buffer) {
            Ok(read) => Ok(read as u32),
            Err(ReadError::InvalidParameter) => Err(STATUS_INVALID_PARAMETER),
            Err(ReadError::Io) => Err(STATUS_DATA_ERROR),
        }
    }

    fn get_file_information(
        &'h self,
        _file_name: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context,
    ) -> OperationResult<FileInfo> {
        println!("get_file_information called");
        if context.ino == 1 {
            Ok(ROOT_FILE_INFO)
        } else {
            let node = get_node(&self.inodes, context.ino).ok_or(STATUS_INVALID_PARAMETER)?;
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
        println!("find_files called");
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
            None => Err(STATUS_INVALID_PARAMETER),
        }
    }

    fn get_disk_free_space(
        &'h self,
        _info: &OperationInfo<'c, 'h, Self>,
    ) -> OperationResult<DiskSpaceInfo> {
        println!("get_disk_free_space called");
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
        println!("get_volume_information called");
        Ok(VolumeInfo {
            name: U16CString::from_str(&self.sku.name).unwrap(),
            serial_number: 0,
            max_component_length: 255,
            fs_flags: 0,
            fs_name: U16CString::from_str("NTFS").unwrap(),
        })
    }

    // fn get_file_security(
    //     &'h self,
    //     file_name: &U16CStr,
    //     security_information: u32,
    //     security_descriptor: PSECURITY_DESCRIPTOR,
    //     buffer_length: u32,
    //     info: &OperationInfo<'c, 'h, Self>,
    //     context: &'c Self::Context,
    // ) -> OperationResult<u32> {
    //     Err(STATUS_NOT_IMPLEMENTED)
    // }

    // fn find_streams(
    //     &'h self,
    //     file_name: &U16CStr,
    //     fill_find_stream_data: impl FnMut(&dokan::FindStreamData) -> dokan::FillDataResult,
    //     info: &OperationInfo<'c, 'h, Self>,
    //     context: &'c Self::Context,
    // ) -> OperationResult<()> {
    //     Err(STATUS_NOT_IMPLEMENTED)
    // }
}
