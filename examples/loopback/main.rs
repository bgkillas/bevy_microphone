use bevy_microphone::{AudioManager, AudioSettings};
use rodio::buffer::SamplesBuffer;
use rodio::{OutputStreamBuilder, Sink};
pub fn main() {
    let audio = AudioSettings::default();
    let sample_rate = audio.sample_rate;
    let mut audio = AudioManager::new(&audio);
    let stream_handle = OutputStreamBuilder::open_default_stream().unwrap();
    let sink = Sink::connect_new(stream_handle.mixer());
    audio.recv_audio_decode(|data| {
        let source = SamplesBuffer::new(1, (sample_rate.get_number() * 1000) as u32, data);
        sink.append(source);
        sink.play()
    });
}
