use audioadapter_buffers::direct::InterleavedSlice;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus::{Application, Channels, Decoder, Encoder};
use rubato::{Fft, FixedSync, Resampler};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;
#[cfg(feature = "log")]
use tracing::*;
#[cfg(feature = "bevy")]
#[derive(bevy_ecs::prelude::Resource)]
pub struct AudioResource(std::sync::Mutex<AudioManager>);
#[cfg(feature = "bevy")]
impl AudioResource {
    pub fn new(audio: &AudioSettings) -> Self {
        Self(AudioManager::new(audio).into())
    }
    pub fn recv_audio<F>(&self, f: F)
    where
        F: FnMut(&[f32]),
    {
        self.0.lock().unwrap().recv_audio(f)
    }
}
pub struct AudioManager {
    rx: Receiver<Vec<u8>>,
    decoder: Decoder,
    kill: Arc<AtomicBool>,
}
impl Drop for AudioManager {
    fn drop(&mut self) {
        self.kill();
    }
}
pub struct AudioSettings {
    input_device: Option<String>,
    disabled: bool,
    channels: Channels,
    frame_size: FrameSize,
    sample_rate: SampleRate,
    application: Application,
}
#[derive(Clone, Copy, Default)]
pub enum SampleRate {
    #[default]
    SR48,
    SR24,
    SR16,
    SR12,
    SR8,
}
impl SampleRate {
    pub fn get_number(self) -> usize {
        match self {
            SampleRate::SR48 => 48,
            SampleRate::SR24 => 24,
            SampleRate::SR16 => 16,
            SampleRate::SR12 => 12,
            SampleRate::SR8 => 8,
        }
    }
}
#[derive(Clone, Copy, Default)]
pub enum FrameSize {
    FS2880,
    FS1920,
    FS960,
    #[default]
    FS480,
    FS240,
    FS120,
}
impl FrameSize {
    pub fn get_number(self) -> usize {
        match self {
            FrameSize::FS2880 => 2880,
            FrameSize::FS1920 => 1920,
            FrameSize::FS960 => 960,
            FrameSize::FS480 => 480,
            FrameSize::FS240 => 240,
            FrameSize::FS120 => 120,
        }
    }
    pub fn size(self, sample_rate: SampleRate) -> usize {
        (self.get_number() * sample_rate.get_number()) / 48
    }
    pub fn time(self) -> usize {
        (self.get_number() * 1000) / 48
    }
}
impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input_device: None,
            disabled: false,
            channels: Channels::Mono,
            frame_size: FrameSize::default(),
            sample_rate: SampleRate::default(),
            application: Application::Audio,
        }
    }
}
impl AudioManager {
    pub fn kill(&self) {
        self.kill.store(true, Ordering::Relaxed);
    }
    pub fn new(settings: &AudioSettings) -> Self {
        let channels = settings.channels;
        let frame_size = settings.frame_size;
        let sample_rate = settings.sample_rate;
        let application = settings.application;
        #[cfg(target_os = "linux")]
        let host = cpal::available_hosts()
            .into_iter()
            .find(|id| *id == cpal::HostId::Jack)
            .and_then(|id| cpal::host_from_id(id).ok())
            .unwrap_or(cpal::default_host());
        #[cfg(not(target_os = "linux"))]
        let host = cpal::default_host();
        let device = {
            let input = settings.input_device.clone();
            if settings.disabled {
                None
            } else if input.is_none() {
                host.default_input_device()
            } else if let Some(d) = host
                .input_devices()
                .map(|mut d| {
                    d.find(|d| {
                        d.description()
                            .ok()
                            .and_then(|a| input.as_ref().map(|i| i == a.name()))
                            .unwrap_or(false)
                    })
                })
                .ok()
                .flatten()
            {
                Some(d)
            } else {
                host.default_input_device()
            }
        };
        let decoder = Decoder::new((sample_rate.get_number() * 1000) as u32, channels).unwrap();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let kill: Arc<AtomicBool> = AtomicBool::new(false).into();
        let kill2 = kill.clone();
        thread::spawn(move || {
            if let Some(device) = device {
                if let Ok(cfg) = device.default_input_config() {
                    let sample = cfg.sample_rate();
                    let channel = cfg.channels();
                    let config = cpal::SupportedStreamConfig::new(
                        if channel <= 2 { channel } else { 2 },
                        sample,
                        *cfg.buffer_size(),
                        cpal::SampleFormat::F32,
                    );
                    let time = frame_size.time() as u64;
                    let frame_size = frame_size.size(sample_rate);
                    if let Ok(mut resamp) = Fft::<f32>::new(
                        sample as usize,
                        sample_rate.get_number() * 1000,
                        frame_size,
                        8,
                        1,
                        FixedSync::Output,
                    ) {
                        let mut encoder = Encoder::new(
                            (sample_rate.get_number() * 1000) as u32,
                            channels,
                            application,
                        )
                        .unwrap();
                        let mut extra = vec![0f32; frame_size];
                        let mut compressed = [0u8; 2048];
                        let mut buffer = [0f32; 2880];
                        match device.build_input_stream(
                            &config.into(),
                            move |data: &[f32], _| {
                                if channel == 1 {
                                    extra.extend(data);
                                } else {
                                    extra.extend(
                                        data.chunks_exact(2)
                                            .map(|a| (a[0] + a[1]) * 0.5)
                                            .collect::<Vec<f32>>(),
                                    )
                                }
                                while extra.len() >= frame_size {
                                    let input =
                                        InterleavedSlice::new(&extra[..frame_size], 1, frame_size)
                                            .unwrap();
                                    let mut output =
                                        InterleavedSlice::new_mut(&mut buffer, 1, frame_size)
                                            .unwrap();
                                    let (_, len) = resamp
                                        .process_into_buffer(&input, &mut output, None)
                                        .unwrap();
                                    if let Ok(len) =
                                        encoder.encode_float(&buffer[..len], &mut compressed)
                                        && len != 0
                                    {
                                        tx.send(compressed[..len].to_vec()).unwrap();
                                    }
                                    extra.drain(..frame_size);
                                }
                            },
                            |_err| {
                                #[cfg(feature = "log")]
                                error!("Stream error: {}", _err)
                            },
                            None,
                        ) {
                            Ok(stream) => {
                                if let Ok(_s) = stream.play() {
                                    loop {
                                        if kill2.load(Ordering::Relaxed) {
                                            return;
                                        }
                                        thread::sleep(Duration::from_micros(time))
                                    }
                                } else {
                                    #[cfg(feature = "log")]
                                    error!("failed to play stream")
                                }
                            }
                            Err(_s) => {
                                #[cfg(feature = "log")]
                                error!(
                                    "no stream {}, {}, {}, {}",
                                    _s,
                                    channel,
                                    cfg.sample_rate(),
                                    cfg.sample_format()
                                )
                            }
                        }
                    } else {
                        #[cfg(feature = "log")]
                        warn!("resamp not found")
                    }
                } else {
                    #[cfg(feature = "log")]
                    warn!("input config not found")
                }
            } else {
                #[cfg(feature = "log")]
                warn!("input device not found")
            }
        });
        Self { rx, decoder, kill }
    }
    pub fn recv_audio<F>(&mut self, mut f: F)
    where
        F: FnMut(&[f32]),
    {
        let out = &mut [0.0; 2048];
        while let Ok(data) = self.rx.try_recv() {
            if let Ok(len) = self.decoder.decode_float(&data, out, false)
                && len != 0
            {
                f(&out[..len])
            }
        }
    }
}
