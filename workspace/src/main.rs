use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use rustysynth::{SoundFont, SynthesizerSettings, ThreadedRender};
use std::fmt::Write;
use std::fs::File;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn main() {
    println!("Read sf2");
    let mut sf2 = File::open("F:\\sf2\\Keppy's Steinway Piano - Concert Grand.sf2").unwrap();
    let sound_font = Arc::new(SoundFont::new(&mut sf2).unwrap());

    let sample_rate = 44100;
    let mut settings = SynthesizerSettings::new(sample_rate);
    settings.maximum_polyphony = 256;

    println!("Loading");
    let mut renderer = ThreadedRender::new(&sound_font, "H:\\U2.mid", settings).unwrap();

    let track_count = renderer.track_count;
    let rendered_track_count = renderer.rendered_track_count.clone();
    rustysynth::rayon::spawn(move || {
        let pb = ProgressBar::new(track_count as u64);
        pb.set_style(ProgressStyle::with_template("{spinner:.green} [{elapsed_precise}] {pos:05}/{len:05} [{wide_bar:.cyan/blue}] {percent}% ({per_sec:<8}) ETA: {eta}")
            .unwrap()
            .with_key("eta", |state: &ProgressState, w: &mut dyn Write| write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap())
            .progress_chars("#>-"));

        let mut progress = 0;
        while progress < track_count {
            progress = rendered_track_count.load(std::sync::atomic::Ordering::SeqCst);
            pb.set_position(progress as u64);

            thread::sleep(Duration::from_millis(100));
        }

        pb.finish();
    });

    let (left, right) = renderer.render();

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: sample_rate as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut writer = hound::WavWriter::create("rendered.wav", spec).unwrap();
    for (left, right) in left.iter().zip(right) {
        writer.write_sample(*left).unwrap();
        writer.write_sample(right).unwrap();
    }
}
