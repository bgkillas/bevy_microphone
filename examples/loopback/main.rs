use bevy_microphone::{AudioManager, AudioSettings, SampleRate};
use rodio::buffer::SamplesBuffer;
use rodio::{OutputStream, OutputStreamBuilder, Sink};
pub fn main() {
    let mut audio = AudioManager::new(&AudioSettings::default());
    let stream_handle = OutputStreamBuilder::open_default_stream().unwrap();
    let sink = Sink::connect_new(stream_handle.mixer());
    loop {
        audio.recv_audio_decode(|data| {
            let source =
                SamplesBuffer::new(1, (SampleRate::default().get_number() * 1000) as u32, data);
            sink.append(source);
            sink.play()
        });
    }
}
