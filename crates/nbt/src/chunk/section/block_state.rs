use alloc::{string::String, vec::Vec};
use nanonbt::{DeserializeNbt, Read, SerializeNbt, Tag};

#[derive(SerializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct BlockStates {
    pub palette: Vec<BlockState>,
    #[nbt(array = long)]
    pub data: Vec<i64>,
}

impl DeserializeNbt for BlockStates {
    const TAG: Tag = Tag::Compound;

    fn deserialize_nbt<R: Read + ?Sized>(r: &mut R) -> Result<Self, nanonbt::Error> {
        let mut palette: Vec<BlockState> = Vec::new();
        let mut data: Vec<i64> = Vec::new();

        loop {
            let tag = Tag::from_u8(r.read_u8()?)?;
            if tag == Tag::End {
                break;
            }
            let name_len = r.read_u16_be()? as usize;
            let mut name_buf = alloc::vec![0u8; name_len];
            r.read_exact(&mut name_buf)?;
            let name = core::str::from_utf8(&name_buf).unwrap_or("");

            match (name, tag) {
                ("palette", Tag::List) => {
                    let elem = Tag::from_u8(r.read_u8()?)?;
                    let n = usize::try_from(r.read_i32_be()?)
                        .map_err(|_| nanonbt::Error::InvalidLength)?;
                    for _ in 0..n {
                        if elem == Tag::Compound {
                            palette.push(BlockState::deserialize_nbt(r)?);
                        } else {
                            nanonbt::skip_nbt(r, elem)?;
                        }
                    }
                }
                ("data", Tag::LongArray) => {
                    let n = usize::try_from(r.read_i32_be()?)
                        .map_err(|_| nanonbt::Error::InvalidLength)?;
                    let mut buf = alloc::vec![0u8; n * 8];
                    r.read_exact(&mut buf)?;
                    for chunk in buf.as_chunks::<8>().0 {
                        data.push(i64::from_be_bytes(*chunk));
                    }
                }
                _ => nanonbt::skip_nbt(r, tag)?,
            }
        }

        Ok(Self { palette, data })
    }
}

#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct BlockState {
    #[nbt(rename = "Name")]
    pub name: String,
    // TODO:
    // #[nbt(rename = "Properties")]
    // properties: Any,
}
