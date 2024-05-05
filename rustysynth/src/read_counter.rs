use std::io::{Read, Seek, SeekFrom};

pub(crate) struct ReadCounter<'a, R: Read + Seek> {
    reader: &'a mut R,
    count: usize,
}

impl<'a, R: Read + Seek> ReadCounter<'a, R> {
    pub(crate) fn new(reader: &'a mut R) -> Self {
        Self { reader, count: 0 }
    }

    pub(crate) fn bytes_read(&self) -> usize {
        self.count
    }
}

impl<'a, R: Read + Seek> Read for ReadCounter<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = self.reader.read(buf)?;
        self.count += len;
        Ok(len)
    }
}

impl<'a, R: Read + Seek> Seek for ReadCounter<'a, R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        match pos {
            SeekFrom::Start(n) => self.count = n as usize,
            SeekFrom::Current(n) => self.count += n as usize,
            _ => unimplemented!() // QwQ
        }
        self.reader.seek(pos)
    }
}
