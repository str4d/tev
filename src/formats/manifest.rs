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
    pub(crate) fn read(path: &Path) -> anyhow::Result<Self> {
        if !path
            .extension()
            .map_or(false, |s| s.eq_ignore_ascii_case("manifest"))
        {
            return Err(anyhow!(
                "Depot manifest file does not have extension .manifest"
            ));
        }

        let mut payload = None;
        let mut metadata = None;
        let mut signature = None;

        let mut file = File::open(path)?;

        loop {
            let read_u32 = |file: &mut File| {
                let mut buf = [0; 4];
                file.read_exact(&mut buf)?;
                Ok::<_, anyhow::Error>(u32::from_le_bytes(buf))
            };

            let read_vec = |file: &mut File| {
                let len = read_u32(file)? as usize;
                let mut buf = vec![0; len];
                file.read_exact(&mut buf)?;
                Ok::<_, anyhow::Error>(buf)
            };

            match read_u32(&mut file)? {
                PROTOBUF_PAYLOAD_MAGIC => {
                    let buf = read_vec(&mut file)?;
                    payload = Some(ContentManifestPayload::parse_from_bytes(&buf)?);
                }
                PROTOBUF_METADATA_MAGIC => {
                    let buf = read_vec(&mut file)?;
                    metadata = Some(ContentManifestMetadata::parse_from_bytes(&buf)?);
                }
                PROTOBUF_SIGNATURE_MAGIC => {
                    let buf = read_vec(&mut file)?;
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
