use bevy_microphone::{AudioManager, AudioSettings};
use rodio::buffer::SamplesBuffer;
use rodio::{ChannelCount, DeviceSinkBuilder, Player, SampleRate};
pub fn main() {
    let audio = AudioSettings::default();
    let sample_rate = audio.sample_rate;
    let mut audio = AudioManager::new(&audio);
    let stream_handle = DeviceSinkBuilder::open_default_sink().unwrap();
    let sink = Player::connect_new(stream_handle.mixer());
    audio.recv_audio_decode(|data| {
        let source = SamplesBuffer::new(
            ChannelCount::new(1).unwrap(),
            SampleRate::new((sample_rate.get_number() * 1000) as u32).unwrap(),
            data,
        );
        sink.append(source);
        sink.play()
    });
}
