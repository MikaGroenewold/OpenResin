//! Small binary encoding helpers.

use crate::{FormatError, Result};

#[derive(Default)]
pub struct BeWriter {
    pub buf: Vec<u8>,
}

impl BeWriter {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            buf: Vec::with_capacity(n),
        }
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }
    /// Fixed-size, NUL padded string.
    pub fn str_fixed(&mut self, s: &str, len: usize) -> &mut Self {
        let b = s.as_bytes();
        let n = b.len().min(len.saturating_sub(1));
        self.buf.extend_from_slice(&b[..n]);
        self.buf.extend(std::iter::repeat_n(0u8, len - n));
        self
    }
}

#[derive(Default)]
pub struct LeWriter {
    pub buf: Vec<u8>,
}

impl LeWriter {
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }
}

/// Cursor over a byte slice with bounds checked reads.
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    big_endian: bool,
}

impl<'a> Reader<'a> {
    pub fn be(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos,
            big_endian: true,
        }
    }
    pub fn le(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos,
            big_endian: false,
        }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(FormatError::Invalid(format!(
                "unexpected end of data at offset {} (need {n} bytes)",
                self.pos
            )));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        let b: [u8; 2] = self.take(2)?.try_into().unwrap();
        Ok(if self.big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    }
    pub fn u32(&mut self) -> Result<u32> {
        let b: [u8; 4] = self.take(4)?.try_into().unwrap();
        Ok(if self.big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    pub fn str_fixed(&mut self, len: usize) -> Result<String> {
        let b = self.take(len)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(len);
        Ok(String::from_utf8_lossy(&b[..end]).into_owned())
    }
}
