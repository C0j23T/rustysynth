#![allow(dead_code)]

use std::cmp;
use std::io::Cursor;
use std::io::Read;
use std::io::Seek;

use rayon::iter::IntoParallelIterator;
use rayon::iter::IntoParallelRefIterator;
use rayon::iter::IntoParallelRefMutIterator;
use rayon::iter::ParallelIterator;

use crate::binary_reader::BinaryReader;
use crate::four_cc::FourCC;
use crate::read_counter::ReadCounter;
use crate::MidiFileError;
use crate::MidiFileLoopType;

#[derive(Clone, Copy)]
#[non_exhaustive]
pub(crate) struct Message {
    pub(crate) channel: u8,
    pub(crate) command: u8,
    pub(crate) data1: u8,
    pub(crate) data2: u8,
}

impl Message {
    pub(crate) const NORMAL: u8 = 0;
    pub(crate) const TEMPO_CHANGE: u8 = 252;
    pub(crate) const LOOP_START: u8 = 253;
    pub(crate) const LOOP_END: u8 = 254;
    pub(crate) const END_OF_TRACK: u8 = 255;

    pub(crate) fn common1(status: u8, data1: u8) -> Self {
        Self {
            channel: status & 0x0F,
            command: status & 0xF0,
            data1,
            data2: 0,
        }
    }

    pub(crate) fn common2(status: u8, data1: u8, data2: u8, loop_type: MidiFileLoopType) -> Self {
        let channel = status & 0x0F;
        let command = status & 0xF0;

        if command == 0xB0 {
            match loop_type {
                MidiFileLoopType::RpgMaker => {
                    if data1 == 111 {
                        return Message::loop_start();
                    }
                }

                MidiFileLoopType::IncredibleMachine => {
                    if data1 == 110 {
                        return Message::loop_start();
                    }
                    if data1 == 111 {
                        return Message::loop_end();
                    }
                }

                MidiFileLoopType::FinalFantasy => {
                    if data1 == 116 {
                        return Message::loop_start();
                    }
                    if data1 == 117 {
                        return Message::loop_end();
                    }
                }

                _ => (),
            }
        }

        Self {
            channel,
            command,
            data1,
            data2,
        }
    }

    pub(crate) fn tempo_change(tempo: i32) -> Self {
        Self {
            channel: Message::TEMPO_CHANGE,
            command: (tempo >> 16) as u8,
            data1: (tempo >> 8) as u8,
            data2: tempo as u8,
        }
    }

    pub(crate) fn loop_start() -> Self {
        Self {
            channel: Message::LOOP_START,
            command: 0,
            data1: 0,
            data2: 0,
        }
    }

    pub(crate) fn loop_end() -> Self {
        Self {
            channel: Message::LOOP_END,
            command: 0,
            data1: 0,
            data2: 0,
        }
    }

    pub(crate) fn end_of_track() -> Self {
        Self {
            channel: Message::END_OF_TRACK,
            command: 0,
            data1: 0,
            data2: 0,
        }
    }

    pub(crate) fn get_message_type(&self) -> u8 {
        match self.channel {
            Message::TEMPO_CHANGE => Message::TEMPO_CHANGE,
            Message::LOOP_START => Message::LOOP_START,
            Message::LOOP_END => Message::LOOP_END,
            Message::END_OF_TRACK => Message::END_OF_TRACK,
            _ => Message::NORMAL,
        }
    }

    pub(crate) fn get_tempo(&self) -> f64 {
        60000000.0
            / (((self.command as i32) << 16) | ((self.data1 as i32) << 8) | (self.data2 as i32))
                as f64
    }
}

/// Represents a standard MIDI file.
#[non_exhaustive]
pub struct MidiFile {
    pub tracks: Vec<MidiTrack>,
    pub(crate) length: f64,
}

impl MidiFile {
    /// Loads a MIDI file from the stream.
    ///
    /// # Arguments
    ///
    /// * `reader` - The data stream used to load the MIDI file.
    pub fn new<R: Read>(reader: &mut R) -> Result<Self, MidiFileError> {
        MidiFile::new_with_loop_type(reader, MidiFileLoopType::LoopPoint(0))
    }

    /// Loads a MIDI file from the stream with a specified loop type.
    ///
    /// # Arguments
    ///
    /// * `reader` - The data stream used to load the MIDI file.
    /// * `loop_type` - The type of the loop extension to be used.
    ///
    /// # Remarks
    ///
    /// `MidiFileLoopType` has the following variants:
    /// * `LoopPoint(usize)` - Specifies the loop start point by a tick value.
    /// * `RpgMaker` - The RPG Maker style loop.
    /// CC #111 will be the loop start point.
    /// * `IncredibleMachine` - The Incredible Machine style loop.
    /// CC #110 and #111 will be the start and end points of the loop.
    /// * `FinalFantasy` - The Final Fantasy style loop.
    /// CC #116 and #117 will be the start and end points of the loop.
    pub fn new_with_loop_type<R: Read>(
        reader: &mut R,
        loop_type: MidiFileLoopType,
    ) -> Result<Self, MidiFileError> {
        let chunk_type = BinaryReader::read_four_cc(reader)?;
        if chunk_type != b"MThd" {
            return Err(MidiFileError::InvalidChunkType {
                expected: FourCC::from_bytes(*b"MThd"),
                actual: chunk_type,
                at: 0,
            });
        }

        let size = BinaryReader::read_i32_big_endian(reader)?;
        if size != 6 {
            return Err(MidiFileError::InvalidChunkData(FourCC::from_bytes(
                *b"MThd",
            )));
        }

        let format = BinaryReader::read_i16_big_endian(reader)?;
        if format != 1 {
            return Err(MidiFileError::UnsupportedFormat(format));
        }

        let track_count = BinaryReader::read_i16_big_endian(reader)? as i32;
        let resolution = BinaryReader::read_i16_big_endian(reader)? as i32;

        let mut cursor = {
            let mut rest_data = Vec::new();
            reader.read_to_end(&mut rest_data)?;
            Cursor::new(rest_data)
        };

        let track_addrs = MidiFile::track_addr(&mut cursor, track_count)?;
        cursor.set_position(0);
        let mut data = Vec::new();
        cursor.read_to_end(&mut data)?;
        drop(cursor);

        let mut tracks_result = track_addrs
            .par_iter()
            .map(|(start, len)| {
                let mut reader = Cursor::new(&data[*start..*start + len]);
                MidiFile::read_track(&mut reader, loop_type)
            })
            .collect::<Vec<Result<Vec<(Message, i32)>, MidiFileError>>>();
        drop(data);

        let mut tracks = Vec::new();
        while let Some(track) = tracks_result.pop() {
            tracks.push(track?);
        }

        let tempo_track = tracks
            .iter()
            .filter(|x| {
                x.iter()
                    .any(|(y, _)| y.get_message_type() == Message::TEMPO_CHANGE)
            })
            .cloned()
            .collect::<Vec<Vec<(Message, i32)>>>();

        if let Some(track) = tempo_track.first() {
            tracks.par_iter_mut().for_each(|x| {
                x.extend(track);
                x.sort_unstable_by(|a, b| a.1.cmp(&b.1));
            });
        }

        match loop_type {
            MidiFileLoopType::LoopPoint(loop_point) if loop_point != 0 => {
                let loop_point = loop_point as i32;
                let track = &mut tracks[0];

                if loop_point <= track.last().unwrap().1 {
                    for i in 0..track.len() {
                        if track[i].1 >= loop_point {
                            track.insert(i, (Message::loop_start(), loop_point));
                            break;
                        }
                    }
                } else {
                    track.push((Message::loop_start(), loop_point));
                }
            }
            _ => (),
        }

        let (tracks, length) = MidiFile::merge_tracks(tracks, resolution);

        Ok(Self { tracks, length })
    }

    fn discard_data<R: Read + Seek>(reader: &mut R) -> Result<(), MidiFileError> {
        let size = BinaryReader::read_i32_variable_length(reader)? as usize;
        BinaryReader::discard_data(reader, size)?;
        Ok(())
    }

    fn read_tempo<R: Read>(reader: &mut R) -> Result<i32, MidiFileError> {
        let size = BinaryReader::read_i32_variable_length(reader)?;
        if size != 3 {
            return Err(MidiFileError::InvalidTempoValue);
        }

        let b1 = BinaryReader::read_u8(reader)? as i32;
        let b2 = BinaryReader::read_u8(reader)? as i32;
        let b3 = BinaryReader::read_u8(reader)? as i32;

        Ok((b1 << 16) | (b2 << 8) | b3)
    }

    pub(crate) fn track_addr<R: Read + Seek>(
        reader: &mut R,
        track_count: i32,
    ) -> Result<Vec<(usize, usize)>, MidiFileError> {
        let mut result = Vec::new();

        let mut index = 0;
        for _ in 0..track_count {
            let chunk_type = BinaryReader::read_four_cc(reader)?;
            if chunk_type != b"MTrk" {
                return Err(MidiFileError::InvalidChunkType {
                    expected: FourCC::from_bytes(*b"MTrk"),
                    actual: chunk_type,
                    at: index as u64,
                });
            }
            let mut size = BinaryReader::read_i32_big_endian(reader)? as usize;
            BinaryReader::discard_data(reader, size)?;

            size += 8;
            result.push((index, size));
            index += size;
        }

        Ok(result)
    }

    pub(crate) fn read_track<R: Read + Seek>(
        reader: &mut R,
        loop_type: MidiFileLoopType,
    ) -> Result<Vec<(Message, i32)>, MidiFileError> {
        let chunk_type = BinaryReader::read_four_cc(reader)?;
        if chunk_type != b"MTrk" {
            return Err(MidiFileError::InvalidChunkType {
                expected: FourCC::from_bytes(*b"MTrk"),
                actual: chunk_type,
                at: reader.stream_position().unwrap_or(0),
            });
        }

        let size = BinaryReader::read_i32_big_endian(reader)? as usize;
        let reader = &mut ReadCounter::new(reader);

        let mut events = Vec::new();

        let mut tick: i32 = 0;
        let mut last_status: u8 = 0;

        loop {
            let delta = BinaryReader::read_i32_variable_length(reader)?;
            let first = BinaryReader::read_u8(reader)?;

            tick += delta;

            if (first & 128) == 0 {
                let command = last_status & 0xF0;
                if command == 0xC0 || command == 0xD0 {
                    events.push((Message::common1(last_status, first), tick));
                } else {
                    let data2 = BinaryReader::read_u8(reader)?;
                    events.push((Message::common2(last_status, first, data2, loop_type), tick));
                }

                continue;
            }

            match first {
                0xF0 => MidiFile::discard_data(reader)?,
                0xF7 => MidiFile::discard_data(reader)?,
                0xFF => match BinaryReader::read_u8(reader)? {
                    0x2F => {
                        BinaryReader::read_u8(reader)?;
                        events.push((Message::end_of_track(), tick));

                        // Some MIDI files may have events inserted after the EOT.
                        // Such events should be ignored.
                        if reader.bytes_read() < size {
                            BinaryReader::discard_data(reader, size - reader.bytes_read())?;
                        }

                        return Ok(events);
                    }
                    0x51 => {
                        events.push((Message::tempo_change(MidiFile::read_tempo(reader)?), tick));
                    }
                    _ => MidiFile::discard_data(reader)?,
                },
                _ => {
                    let command = first & 0xF0;
                    if command == 0xC0 || command == 0xD0 {
                        let data1 = BinaryReader::read_u8(reader)?;
                        events.push((Message::common1(first, data1), tick));
                    } else {
                        let data1 = BinaryReader::read_u8(reader)?;
                        let data2 = BinaryReader::read_u8(reader)?;
                        events.push((Message::common2(first, data1, data2, loop_type), tick));
                    }
                }
            }

            last_status = first
        }
    }

    pub(crate) fn cast_delta(track: Vec<(Message, i32)>, resolution: i32) -> (MidiTrack, f64) {
        if track.is_empty() {
            return (
                MidiTrack {
                    messages: Vec::new(),
                    times: Vec::new(),
                },
                0.0,
            );
        }

        let mut messages = Vec::new();
        let mut times = Vec::new();

        let mut index = 0;

        let mut current_tick: i32 = 0;
        let mut current_time: f64 = 0.0;

        let mut tempo: f64 = 120.0;

        loop {
            if index >= track.len() {
                break;
            }

            let next_tick = track[index].1;
            let delta_tick = next_tick - current_tick;
            let delta_time = 60.0 / (resolution as f64 * tempo) * delta_tick as f64;

            current_tick += delta_tick;
            current_time += delta_time;

            let message = track[index].0;
            if message.get_message_type() == Message::TEMPO_CHANGE {
                tempo = message.get_tempo();
            } else {
                messages.push(message);
                times.push(current_time);
            }

            index += 1;
        }

        (MidiTrack { messages, times }, current_time)
    }

    fn merge_tracks(tracks: Vec<Vec<(Message, i32)>>, resolution: i32) -> (Vec<MidiTrack>, f64) {
        let tracks = tracks
            .into_par_iter()
            .map(|track| MidiFile::cast_delta(track, resolution))
            .collect::<Vec<(MidiTrack, f64)>>();

        let length = if let Some((_, len)) = tracks
            .par_iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(cmp::Ordering::Equal))
        {
            *len
        } else {
            0.0
        };

        let tracks = tracks
            .into_iter()
            .map(|(track, _)| track)
            .collect::<Vec<MidiTrack>>();

        (tracks, length)
    }

    /// Get the length of the MIDI file in seconds.
    pub fn get_length(&self) -> f64 {
        self.length
    }
}

#[non_exhaustive]
pub struct MidiTrack {
    pub(crate) messages: Vec<Message>,
    pub(crate) times: Vec<f64>,
}

impl MidiTrack {
    pub fn get_length(&self) -> f64 {
        *self.times.last().unwrap()
    }
}
