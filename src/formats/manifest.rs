use std::io::Write;
use std::path::Path;
use std::{fs::File, io::Read};

use anyhow::anyhow;
use base64::{engine::general_purpose::STANDARD, Engine};
use steam_vent::proto::{
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

    pub(crate) fn write<W: Write>(&self, mut writer: W) -> anyhow::Result<()> {
        let write_vec = |writer: &mut W, v: Vec<u8>| {
            writer.write_all(&(v.len() as u32).to_le_bytes())?;
            writer.write_all(&v)
        };

        writer.write_all(&PROTOBUF_PAYLOAD_MAGIC.to_le_bytes())?;
        write_vec(&mut writer, self.payload.write_to_bytes()?)?;

        writer.write_all(&PROTOBUF_METADATA_MAGIC.to_le_bytes())?;
        write_vec(&mut writer, self.metadata.write_to_bytes()?)?;

        writer.write_all(&PROTOBUF_SIGNATURE_MAGIC.to_le_bytes())?;
        write_vec(&mut writer, self.signature.write_to_bytes()?)?;

        writer.write_all(&PROTOBUF_ENDOFMANIFEST_MAGIC.to_le_bytes())?;

        Ok(())
    }

    pub(crate) fn decrypt_filenames(&mut self, depot_key: &[u8; 32]) -> anyhow::Result<()> {
        if self.metadata.filenames_encrypted() {
            for mapping in &mut self.payload.mappings {
                mapping.set_filename(decrypt_string(mapping.filename(), depot_key)?);
                if mapping.has_linktarget() {
                    mapping.set_linktarget(decrypt_string(mapping.linktarget(), depot_key)?);
                }
            }

            self.metadata.set_filenames_encrypted(false);
        }

        Ok(())
    }
}

fn decrypt_string(s: &str, depot_key: &[u8; 32]) -> anyhow::Result<String> {
    let encoded = s.lines().fold(String::new(), |acc, line| acc + line);
    let ciphertext = STANDARD.decode(&encoded)?;
    let plaintext =
        steam_vent_crypto::symmetric_decrypt_without_hmac(ciphertext.as_slice().into(), depot_key)?;
    Ok(String::from_utf8(plaintext.to_vec())?)
}
