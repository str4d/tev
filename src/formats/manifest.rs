use std::path::Path;
use std::{fs::File, io::Read};

use anyhow::anyhow;
use steam_vent_proto::{
    content_manifest::{ContentManifestMetadata, ContentManifestPayload, ContentManifestSignature},
    protobuf::Message,
};

const PROTOBUF_PAYLOAD_MAGIC: u32 = 0x71F617D0;
const PROTOBUF_METADATA_MAGIC: u32 = 0x1F4812BE;
const PROTOBUF_SIGNATURE_MAGIC: u32 = 0x1B81B817;
const PROTOBUF_ENDOFMANIFEST_MAGIC: u32 = 0x32C415AB;

#[derive(Debug)]
pub(crate) struct Manifest {
    pub(crate) payload: ContentManifestPayload,
    pub(crate) metadata: ContentManifestMetadata,
    pub(crate) signature: ContentManifestSignature,
}

impl Manifest {
    pub(crate) fn open(path: &Path) -> anyhow::Result<Self> {
        if !path
            .extension()
            .map_or(false, |s| s.eq_ignore_ascii_case("manifest"))
        {
            return Err(anyhow!(
                "Depot manifest file does not have extension .manifest"
            ));
        }

        let file = File::open(path)?;

        Self::read(file)
    }

    pub(crate) fn read<R: Read>(mut reader: R) -> anyhow::Result<Self> {
        let mut payload = None;
        let mut metadata = None;
        let mut signature = None;

        loop {
            let read_u32 = |reader: &mut R| {
                let mut buf = [0; 4];
                reader.read_exact(&mut buf)?;
                Ok::<_, anyhow::Error>(u32::from_le_bytes(buf))
            };

            let read_vec = |reader: &mut R| {
                let len = read_u32(reader)? as usize;
                let mut buf = vec![0; len];
                reader.read_exact(&mut buf)?;
                Ok::<_, anyhow::Error>(buf)
            };

            match read_u32(&mut reader)? {
                PROTOBUF_PAYLOAD_MAGIC => {
                    let buf = read_vec(&mut reader)?;
                    payload = Some(ContentManifestPayload::parse_from_bytes(&buf)?);
                }
                PROTOBUF_METADATA_MAGIC => {
                    let buf = read_vec(&mut reader)?;
                    metadata = Some(ContentManifestMetadata::parse_from_bytes(&buf)?);
                }
                PROTOBUF_SIGNATURE_MAGIC => {
                    let buf = read_vec(&mut reader)?;
                    signature = Some(ContentManifestSignature::parse_from_bytes(&buf)?);
                }
                PROTOBUF_ENDOFMANIFEST_MAGIC => break,
                n => return Err(anyhow!("Unrecognized magic value {n} in depot manifest")),
            }
        }

        payload
            .zip(metadata)
            .zip(signature)
            .map(|((payload, metadata), signature)| Manifest {
                payload,
                metadata,
                signature,
            })
            .ok_or(anyhow!("Missing manifest components"))
    }
}
