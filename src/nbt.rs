//! Minimal little-endian NBT reader for Bedrock `level.dat`.
//!
//! Bedrock uses the same tag set as Java NBT but every numeric field is
//! little-endian and the payload is uncompressed. `level.dat` prefixes the
//! NBT with an 8-byte header: `i32 storage_version` + `i32 payload_length`.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone)]
#[allow(dead_code)] // full tag set parsed for correctness; not all payloads are consumed yet
pub enum Value {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<Value>),
    Compound(HashMap<String, Value>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Value {
    pub fn as_compound(&self) -> Option<&HashMap<String, Value>> {
        match self {
            Value::Compound(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Byte(v) => Some(*v as i64),
            Value::Short(v) => Some(*v as i64),
            Value::Int(v) => Some(*v as i64),
            Value::Long(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(v) => Some(v),
            _ => None,
        }
    }
}

pub struct LevelDat {
    pub storage_version: i32,
    pub root: Value,
}

/// Parse a full `level.dat` file (8-byte header + LE NBT compound).
pub fn parse_level_dat(data: &[u8]) -> Result<LevelDat> {
    if data.len() < 8 {
        bail!("file too short for level.dat header ({} bytes)", data.len());
    }
    let storage_version = i32::from_le_bytes(data[0..4].try_into().unwrap());
    let payload_len = i32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let payload = &data[8..];
    if payload_len != payload.len() {
        // Tolerate trailing garbage but never a short payload.
        if payload_len > payload.len() {
            bail!(
                "header claims {payload_len} payload bytes but only {} present",
                payload.len()
            );
        }
    }
    let mut r = Reader { data: &payload[..payload_len], pos: 0 };
    let (_name, root) = r.read_named_tag().context("reading root tag")?;
    if root.as_compound().is_none() {
        bail!("root tag is not a compound");
    }
    Ok(LevelDat { storage_version, root })
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            bail!("unexpected end of NBT data at offset {}", self.pos);
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn string(&mut self) -> Result<String> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        // Mojang strings are nominally UTF-8; be lossy rather than fail a scan.
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    fn read_named_tag(&mut self) -> Result<(String, Value)> {
        let tag = self.u8()?;
        if tag == 0 {
            bail!("unexpected TAG_End at top level");
        }
        let name = self.string()?;
        let value = self.payload(tag).with_context(|| format!("in tag '{name}'"))?;
        Ok((name, value))
    }

    fn payload(&mut self, tag: u8) -> Result<Value> {
        Ok(match tag {
            1 => Value::Byte(self.u8()? as i8),
            2 => Value::Short(self.i16()?),
            3 => Value::Int(self.i32()?),
            4 => Value::Long(self.i64()?),
            5 => Value::Float(self.f32()?),
            6 => Value::Double(self.f64()?),
            7 => {
                let len = self.i32()?;
                if len < 0 {
                    bail!("negative byte-array length");
                }
                Value::ByteArray(self.take(len as usize)?.to_vec())
            }
            8 => Value::String(self.string()?),
            9 => {
                let elem_tag = self.u8()?;
                let len = self.i32()?;
                if len < 0 {
                    bail!("negative list length");
                }
                let mut items = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    items.push(self.payload(elem_tag)?);
                }
                Value::List(items)
            }
            10 => {
                let mut map = HashMap::new();
                loop {
                    let child_tag = self.u8()?;
                    if child_tag == 0 {
                        break;
                    }
                    let name = self.string()?;
                    let value =
                        self.payload(child_tag).with_context(|| format!("in tag '{name}'"))?;
                    map.insert(name, value);
                }
                Value::Compound(map)
            }
            11 => {
                let len = self.i32()?;
                if len < 0 {
                    bail!("negative int-array length");
                }
                let mut items = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    items.push(self.i32()?);
                }
                Value::IntArray(items)
            }
            12 => {
                let len = self.i32()?;
                if len < 0 {
                    bail!("negative long-array length");
                }
                let mut items = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    items.push(self.i64()?);
                }
                Value::LongArray(items)
            }
            t => bail!("unknown NBT tag id {t} at offset {}", self.pos - 1),
        })
    }
}
