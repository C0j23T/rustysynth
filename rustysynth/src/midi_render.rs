use std::{
    fs::File,
    io::{Cursor, Read, Seek},
    sync::{atomic::AtomicI32, Arc, Mutex},
};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rayon::prelude::ParallelSliceMut;

use crate::{
    array_math::ArrayMath, binary_reader::BinaryReader, four_cc::FourCC, midifile::*,
    MidiFileError, MidiFileLoopType, MidiFileSequencer, SoundFont, Synthesizer,
    SynthesizerSettings,
};

pub struct ThreadedRender<'a> {
    file: &'a str,
    sound_font: Arc<SoundFont>,

    resolution: i32,

    tempo_map: Vec<(Message, i32)>,
    track_addr: Vec<(usize, usize)>,

    synthesizer_settings: SynthesizerSettings,

    pub track_count: i32,
    pub rendered_track_count: Arc<AtomicI32>,
}

impl<'a> ThreadedRender<'a> {
    pub fn new(
        sound_font: &Arc<SoundFont>,
        file: &'a str,
        synthesizer_settings: SynthesizerSettings,
    ) -> Result<Self, MidiFileError> {
        let mut reader = File::open(file)?;

        let chunk_type = BinaryReader::read_four_cc(&mut reader)?;
        if chunk_type != b"MThd" {
            return Err(MidiFileError::InvalidChunkType {
                expected: FourCC::from_bytes(*b"MThd"),
                actual: chunk_type,
                at: reader.stream_position().unwrap_or(0),
            });
        }

        let size = BinaryReader::read_i32_big_endian(&mut reader)?;
        if size != 6 {
            return Err(MidiFileError::InvalidChunkData(FourCC::from_bytes(
                *b"MThd",
            )));
        }

        let format = BinaryReader::read_i16_big_endian(&mut reader)?;
        if !(format == 0 || format == 1) {
            return Err(MidiFileError::UnsupportedFormat(format));
        }

        let track_count = BinaryReader::read_u16_big_endian(&mut reader)? as i32;
        let resolution = BinaryReader::read_i16_big_endian(&mut reader)? as i32;

        let mut tempo_map = None;
        while let Ok(track) = MidiFile::read_track(&mut reader, MidiFileLoopType::LoopPoint(0)) {
            if track
                .iter()
                .any(|(msg, _)| msg.get_message_type() == Message::TEMPO_CHANGE)
            {
                tempo_map = Some(track);
                break;
            }
        }
        if tempo_map.is_none() {
            return Err(MidiFileError::UnsupportedFormat(format));
        }

        let track_addr = {
            let mut reader = File::open(file)?;
            reader.seek(std::io::SeekFrom::Current(0xe))?;
            MidiFile::track_addr(&mut reader, track_count)?
        };

        Ok(Self {
            file,
            resolution,
            sound_font: Arc::clone(&sound_font),
            synthesizer_settings,
            track_addr,
            tempo_map: tempo_map.unwrap(),
            track_count,
            rendered_track_count: Arc::new(AtomicI32::new(0)),
        })
    }

    pub fn render(&mut self) -> (Vec<f32>, Vec<f32>) {
        let loop_type = MidiFileLoopType::LoopPoint(0);

        let master_left: Mutex<Vec<f32>> = Mutex::new(Vec::new());
        let master_right: Mutex<Vec<f32>> = Mutex::new(Vec::new());

        self.track_addr
            .par_iter()
            .for_each(|(start, size)| {
                let mut reader = {
                    let mut file = File::open(self.file).unwrap();
                    file.seek(std::io::SeekFrom::Current(0xe)).unwrap();
                    file
                        .seek(std::io::SeekFrom::Current(*start as i64))
                        .unwrap();
                    let mut buf = vec![0; *size];
                    file.read_exact(&mut buf).unwrap();
                    Cursor::new(buf)
                };

                let mut track = MidiFile::read_track(&mut reader, loop_type).unwrap();
                track.extend(self.tempo_map.iter());
                track.par_sort_by(|a, b| a.1.cmp(&b.1));

                let (casted, _) = MidiFile::cast_delta(track, self.resolution);

                let synthesizer =
                    Synthesizer::new(&self.sound_font, &self.synthesizer_settings).unwrap();
                let mut sequencer = MidiFileSequencer::new(synthesizer);
                let length = casted.get_length();
                sequencer.play(casted, false);

                let sample_count = (self.synthesizer_settings.sample_rate as f64 * length) as usize;
                let mut left: Vec<f32> = vec![0_f32; sample_count];
                let mut right: Vec<f32> = vec![0_f32; sample_count];

                sequencer.render(&mut left[..], &mut right[..]);

                {
                    let mut left_handler = master_left.lock().unwrap();
                    let len = left_handler.len();
                    if len < left.len() {
                        left_handler.resize(left.len(), 0.0);
                    }
                    ArrayMath::sum(&left, &mut left_handler);
                }

                {
                    let mut right_handler = master_right.lock().unwrap();
                    let len = right_handler.len();
                    if len < right.len() {
                        right_handler.resize(right.len(), 0.0);
                    }
                    ArrayMath::sum(&right, &mut right_handler);
                }

                self.rendered_track_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });

        (
            master_left.into_inner().unwrap(),
            master_right.into_inner().unwrap(),
        )
    }
}
