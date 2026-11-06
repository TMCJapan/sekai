pub(crate) mod chunk;

use nanonbt::{Read, SliceReader, Tag, read_string_into, skip_nbt};
use sekai_util::{ChunkHash, RegionKind};

use crate::error::{ChunkHashError, ChunkNbtError};
use chunk::{Chunk, ChunkPayload, LegacyChunkPayload};

/// Deserializes a level-chunk NBT root, either format.
///
/// The 1.13+ shape is tried first: it is the common case, and trying it
/// costs nothing extra there. A legacy chunk is exactly the case where that
/// read fails and the pre-1.13 `Level` wrapper decodes instead.
///
/// When both fail, the error names the field the payload was actually
/// missing: a root with no `Level` member is a modern chunk that failed for
/// its own reason, so only that case reports the legacy read's failure.
pub(crate) fn parse(bytes: &[u8]) -> Result<Chunk<'_>, ChunkNbtError> {
    let modern = match nanonbt::from_slice::<ChunkPayload>(bytes) {
        Ok(modern) => return Ok(Chunk::Modern(modern)),
        Err(modern) => modern,
    };
    match nanonbt::from_slice::<LegacyChunkPayload>(bytes) {
        Ok(legacy) => Ok(Chunk::Legacy(legacy)),
        Err(legacy) if matches!(modern, nanonbt::Error::MissingField("Level")) => {
            Err(legacy.into())
        }
        Err(_) => Err(modern.into()),
    }
}

/// The CAS key of an already-decompressed chunk payload.
///
/// A `region/` payload keys on the [`Chunk::normalized_hash`] digest:
/// volatile tags (`InhabitedTime`, `LastUpdate`) never reach it, so a server
/// rewrite of those tags does not store the chunk again.
///
/// `entities`/`poi` payloads have no level-chunk schema to normalize, so they
/// key on their exact bytes. That identity is a property of the region kind,
/// not of a failed decode: the payload must still be well-formed NBT, and
/// anything else is rejected.
///
/// Nothing is keyed by default. A payload that is not well-formed NBT, or a
/// `region/` payload whose schema cannot be read, is an error rather than a
/// raw-byte fallback, so a damaged chunk cannot hide behind a byte-level
/// identity that later normalizes differently.
///
/// # Errors
///
/// [`ChunkHashError::Invalid`] when the payload is not well-formed NBT, or
/// when a `region/` payload decodes as neither the 1.13+ nor the legacy
/// level-chunk shape, and [`ChunkHashError::Normalize`] when the decoded
/// chunk cannot be encoded for its normalized view.
pub(crate) fn chunk_key(kind: RegionKind, raw_nbt: &[u8]) -> Result<ChunkHash, ChunkHashError> {
    match kind {
        RegionKind::Region => {
            let chunk = parse(raw_nbt).map_err(ChunkHashError::Invalid)?;
            chunk
                .normalized_hash()
                .map_err(|source| ChunkHashError::Normalize(source.into()))
        }
        // These kinds carry their own schema, so byte identity is their
        // defined key. It still requires valid NBT: a truncated or non-NBT
        // payload has no hash to record.
        RegionKind::Entities | RegionKind::Poi => {
            validate_nbt(raw_nbt).map_err(ChunkHashError::Invalid)?;
            Ok(ChunkHash::new(sekai_util::hash_bytes(raw_nbt)))
        }
    }
}

/// Whether `raw_nbt` is one complete, well-formed NBT compound.
///
/// `entities`/`poi` documents are compounds at the root, so the body must be
/// fully traversable as NBT. No level-chunk schema is applied.
fn validate_nbt(raw_nbt: &[u8]) -> Result<(), ChunkNbtError> {
    let mut reader = SliceReader::new(raw_nbt);
    let tag = reader.read_u8()?;
    let mut root_name = alloc::string::String::new();
    if tag != Tag::Compound.as_u8() {
        return Err(nanonbt::Error::InvalidTag {
            expected: Tag::Compound,
            actual: tag,
        }
        .into());
    }
    read_string_into(&mut reader, &mut root_name)?;
    skip_nbt(&mut reader, Tag::Compound)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use nanonbt::Cesu8;

    use super::chunk::{BlockTick, FluidTick, Heightmaps, Section};
    use super::*;

    #[test]
    fn normalized_hash_is_deterministic_and_skips_volatile_tags() {
        let payload = |inhabited_time, last_update| ChunkPayload {
            last_update,
            inhabited_time,
            ..ChunkPayload::empty_full()
        };

        let base = payload(7, 9);
        let base_hash = base.normalized_hash().unwrap();
        assert_eq!(base_hash, payload(7, 9).normalized_hash().unwrap());
        assert_eq!(base_hash, payload(999, 1_234).normalized_hash().unwrap());
    }

    #[test]
    fn volatile_only_change_reports_nothing() {
        let payload = |inhabited_time, last_update| ChunkPayload {
            last_update,
            inhabited_time,
            ..ChunkPayload::empty_full()
        };

        let old = Chunk::Modern(payload(7, 9));
        let new = Chunk::Modern(payload(999, 1_234));
        assert_eq!(
            old.normalized_hash().unwrap(),
            new.normalized_hash().unwrap()
        );
    }

    #[test]
    fn payload_round_trips() {
        let payload = ChunkPayload {
            x_pos: 3,
            z_pos: -1,
            last_update: 9,
            inhabited_time: 7,
            ..ChunkPayload::empty_full()
        };
        let bytes = nanonbt::to_bytes(&payload).unwrap();
        let parsed = nanonbt::from_slice::<ChunkPayload>(&bytes).unwrap();
        assert_eq!(payload, parsed);
    }

    #[test]
    fn blob_hash_ignores_volatile_tags_and_tracks_content() {
        let status = Cesu8::try_from_str("minecraft:full").unwrap();
        let bytes = |inhabited_time, last_update, status| {
            nanonbt::to_bytes(&ChunkPayload {
                status,
                last_update,
                inhabited_time,
                ..ChunkPayload::empty_full()
            })
            .unwrap()
        };

        let empty = Cesu8::from_str("minecraft:empty");
        let base = bytes(7, 9, status);
        let other = bytes(7, 9, &empty);
        assert_eq!(
            chunk_key(RegionKind::Region, &base).unwrap(),
            chunk_key(RegionKind::Region, &bytes(999, 1_234, status)).unwrap()
        );
        assert_ne!(
            chunk_key(RegionKind::Region, &base).unwrap(),
            chunk_key(RegionKind::Region, &other).unwrap()
        );
    }

    /// A `region/` payload that is not a level chunk has no normalized view,
    /// so it gets no key: keying it on its exact bytes would hide a damaged
    /// chunk behind a byte-level identity that never normalizes again.
    #[test]
    fn region_payload_without_a_chunk_schema_has_no_key() {
        assert!(matches!(
            chunk_key(RegionKind::Region, b"entities-payload"),
            Err(ChunkHashError::Invalid(_))
        ));

        // Valid NBT (an empty compound), but neither level-chunk shape.
        assert!(matches!(
            chunk_key(RegionKind::Region, &[10u8, 0, 0, 0]),
            Err(ChunkHashError::Invalid(_))
        ));
    }

    /// `entities`/`poi` key on exact bytes, which is their defined identity -
    /// but only once the payload is well-formed NBT.
    #[test]
    fn sparse_kinds_key_on_validated_bytes() {
        let doc = alloc::vec![
            10u8, 0, 0, // root compound, empty name
            8, 0, 2, b'i', b'd', 0, 1, b'a', // id = "a"
            0,    // end
        ];
        let other = alloc::vec![
            10u8, 0, 0, // root compound, empty name
            8, 0, 2, b'i', b'd', 0, 1, b'b', // id = "b"
            0,    // end
        ];

        let key = chunk_key(RegionKind::Entities, &doc).unwrap();
        assert_eq!(key, chunk_key(RegionKind::Poi, &doc).unwrap());
        assert_eq!(key, ChunkHash::new(sekai_util::hash_bytes(&doc)));
        assert_ne!(key, chunk_key(RegionKind::Entities, &other).unwrap());

        assert!(matches!(
            chunk_key(RegionKind::Entities, b"not nbt"),
            Err(ChunkHashError::Invalid(_))
        ));
        // A root compound whose body is cut short is not well-formed.
        assert!(matches!(
            chunk_key(RegionKind::Entities, &doc[..doc.len() - 1]),
            Err(ChunkHashError::Invalid(_))
        ));
    }

    #[test]
    fn section_reads_vanilla_light_keys_and_defaults_missing_fields() {
        use nanonbt::SerializeNbt;

        #[derive(SerializeNbt)]
        struct MinimalSection {
            #[nbt(rename = "Y")]
            y: i8,
        }

        let bytes = nanonbt::to_bytes(&MinimalSection { y: 3 }).unwrap();
        let section: Section = nanonbt::from_slice(&bytes).unwrap();
        assert_eq!(section.y, 3);
        assert_eq!(section.block_lights.len(), 0);
        assert_eq!(section.sky_lights.len(), 0);

        let section = Section {
            y: -2,
            block_lights: &[1, 2, 3],
            sky_lights: &[4, 5],
            ..Section::default()
        };
        let bytes = nanonbt::to_bytes(&section).unwrap();
        assert!(bytes.windows(10).any(|window| window == b"BlockLight"));
        assert!(bytes.windows(8).any(|window| window == b"SkyLight"));
        assert!(!bytes.windows(11).any(|window| window == b"BlockLights"));

        let parsed: Section = nanonbt::from_slice(&bytes).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn normalized_hash_covers_both_tick_lists() {
        let id = Cesu8::try_from_str("minecraft:water").unwrap();
        let tick = BlockTick {
            x: 1,
            y: 2,
            z: 3,
            tick: 4,
            priority: 5,
            id,
        };
        let payload = |block_ticks, fluid_ticks| ChunkPayload {
            fluid_ticks,
            block_ticks,
            ..ChunkPayload::empty_full()
        };

        let empty = payload(alloc::vec![], alloc::vec![]);
        let block = payload(alloc::vec![tick.clone()], alloc::vec![]);
        let fluid = payload(alloc::vec![], alloc::vec![tick]);

        assert_ne!(
            empty.normalized_hash().unwrap(),
            block.normalized_hash().unwrap()
        );
        assert_ne!(
            empty.normalized_hash().unwrap(),
            fluid.normalized_hash().unwrap()
        );
    }

    /// One named tag: type byte, length-prefixed name, then `payload`.
    fn named_tag(kind: Tag, key: &str, payload: &[u8]) -> alloc::vec::Vec<u8> {
        let mut out = alloc::vec![kind.as_u8()];
        out.extend(u16::try_from(key.len()).unwrap().to_be_bytes());
        out.extend(key.as_bytes());
        out.extend(payload);
        out
    }

    /// A compound holding the single `Int` tag `key = value`.
    fn int_compound(key: &str, value: i32) -> alloc::vec::Vec<u8> {
        let mut out = named_tag(Tag::Int, key, &value.to_be_bytes());
        out.push(Tag::End.as_u8());
        out
    }

    /// Only the keys `ChunkPayload` requires. Encoding through
    /// `ChunkPayload` itself would also write `isLightOn` and
    /// `PostProcessing`, so a spliced copy would be a duplicate key and a
    /// "missing" one would not be missing.
    #[derive(nanonbt::SerializeNbt)]
    struct RequiredKeys<'a> {
        #[nbt(rename = "DataVersion")]
        data_version: i32,
        #[nbt(rename = "xPos")]
        x_pos: i32,
        #[nbt(rename = "zPos")]
        z_pos: i32,
        #[nbt(rename = "yPos")]
        y_pos: i32,
        #[nbt(rename = "Status")]
        status: &'a Cesu8,
        #[nbt(rename = "LastUpdate")]
        last_update: i64,
        sections: &'a [Section<'a>],
        block_entities: &'a [super::chunk::BlockEntity<'a>],
        #[nbt(rename = "Heightmaps")]
        heightmaps: Heightmaps<'a>,
        fluid_ticks: &'a [FluidTick<'a>],
        block_ticks: &'a [BlockTick<'a>],
        #[nbt(rename = "InhabitedTime")]
        inhabited_time: i64,
    }

    /// A minimal modern chunk with `extra` spliced in before the root's
    /// `TAG_End`, for keys whose opaque bodies a struct literal cannot build.
    fn chunk_with(extra: &[&[u8]]) -> alloc::vec::Vec<u8> {
        let mut bytes = nanonbt::to_bytes(&RequiredKeys {
            data_version: 1,
            x_pos: 3,
            z_pos: -1,
            y_pos: 0,
            status: &Cesu8::from_str("minecraft:full"),
            last_update: 9,
            sections: &[],
            block_entities: &[],
            heightmaps: Heightmaps::default(),
            fluid_ticks: &[],
            block_ticks: &[],
            inhabited_time: 7,
        })
        .unwrap();
        assert_eq!(bytes.pop(), Some(Tag::End.as_u8()));
        bytes.extend(extra.concat());
        bytes.push(Tag::End.as_u8());
        bytes
    }

    fn post_processing(packed: i16) -> alloc::vec::Vec<u8> {
        let mut inner = alloc::vec![Tag::Short.as_u8()];
        inner.extend(1i32.to_be_bytes());
        inner.extend(packed.to_be_bytes());
        let mut outer = alloc::vec![Tag::List.as_u8()];
        outer.extend(1i32.to_be_bytes());
        outer.extend(inner);
        named_tag(Tag::List, "PostProcessing", &outer)
    }

    fn region_key(bytes: &[u8]) -> ChunkHash {
        chunk_key(RegionKind::Region, bytes).unwrap()
    }

    /// `structures`, `PostProcessing`, `blending_data` and `isLightOn` used
    /// to be skipped on decode, so a change to only one of them kept the
    /// old key and was never stored.
    #[test]
    fn each_tracked_root_key_changes_the_chunk_key() {
        let variants = [
            named_tag(Tag::Compound, "structures", &int_compound("x", 1)),
            named_tag(Tag::Compound, "structures", &int_compound("x", 2)),
            post_processing(1),
            post_processing(2),
            named_tag(
                Tag::Compound,
                "blending_data",
                &int_compound("min_section", -4),
            ),
            named_tag(
                Tag::Compound,
                "blending_data",
                &int_compound("min_section", 0),
            ),
            named_tag(Tag::Byte, "isLightOn", &[1]),
        ];

        let mut keys: alloc::vec::Vec<ChunkHash> = variants
            .iter()
            .map(|tag| region_key(&chunk_with(&[tag])))
            .collect();
        keys.push(region_key(&chunk_with(&[])));
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "every variant must key apart");
    }

    /// 26.4 renames `Status` to `status`; a chunk saved only to pick up the
    /// new spelling keys exactly as it did before.
    #[test]
    fn lowercase_status_keys_like_the_legacy_spelling() {
        let pre_rename = chunk_with(&[]);
        let old_name = named_tag(Tag::String, "Status", &[]);
        let new_name = named_tag(Tag::String, "status", &[]);
        let at = pre_rename
            .windows(old_name.len())
            .position(|window| window == old_name.as_slice())
            .unwrap();
        let mut renamed = pre_rename.clone();
        renamed[at..at + new_name.len()].copy_from_slice(&new_name);

        assert_eq!(region_key(&pre_rename), region_key(&renamed));
    }

    /// With both spellings present, neither value can be picked safely.
    #[test]
    fn both_status_spellings_have_no_key() {
        let mut status = 14u16.to_be_bytes().to_vec();
        status.extend(b"minecraft:full");
        let both = chunk_with(&[&named_tag(Tag::String, "status", &status)]);
        assert!(matches!(
            chunk_key(RegionKind::Region, &both),
            Err(ChunkHashError::Invalid(_))
        ));
    }

    /// An absent compound and a present empty one are different documents,
    /// so they must not share a key.
    #[test]
    fn absent_and_empty_structures_key_apart() {
        let empty = named_tag(Tag::Compound, "structures", &[Tag::End.as_u8()]);
        assert_ne!(
            region_key(&chunk_with(&[])),
            region_key(&chunk_with(&[&empty]))
        );
    }

    /// `isLightOn` reads as a flag, so an explicit `0` is the same chunk as
    /// a missing key.
    #[test]
    fn explicit_light_off_matches_missing_flag() {
        let off = named_tag(Tag::Byte, "isLightOn", &[0]);
        assert_eq!(
            region_key(&chunk_with(&[])),
            region_key(&chunk_with(&[&off]))
        );
    }

    #[test]
    fn tracked_root_keys_round_trip() {
        let structures = named_tag(Tag::Compound, "structures", &int_compound("x", 1));
        let blending = named_tag(
            Tag::Compound,
            "blending_data",
            &int_compound("min_section", -4),
        );
        let light = named_tag(Tag::Byte, "isLightOn", &[1]);
        let bytes = chunk_with(&[&structures, &post_processing(5), &blending, &light]);

        let parsed = nanonbt::from_slice::<ChunkPayload>(&bytes).unwrap();
        assert!(!parsed.structures.is_absent());
        assert!(!parsed.blending_data.is_absent());
        assert_eq!(parsed.post_processing, alloc::vec![alloc::vec![5i16]]);
        assert!(parsed.is_light_on);

        let reencoded = nanonbt::to_bytes(&parsed).unwrap();
        assert_eq!(
            nanonbt::from_slice::<ChunkPayload>(&reencoded).unwrap(),
            parsed
        );
    }
}
