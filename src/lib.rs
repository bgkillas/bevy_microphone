use audioadapter_buffers::direct::InterleavedSlice;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus::{Application, Channels, Decoder, Encoder};
use rubato::{Fft, FixedSync, Resampler};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use tracing::*;
#[cfg(feature = "bevy")]
#[derive(bevy_ecs::prelude::Resource)]
pub struct AudioResource(Mutex<AudioManager>);
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
    frame_size: usize,
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
    frame_size: usize,
    sample_rate: usize,
}
impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input_device: None,
            disabled: false,
            channels: Channels::Mono,
            frame_size: 960,
            sample_rate: 48000,
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
        let decoder = Decoder::new(sample_rate as u32, channels).unwrap();
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
                    if let Ok(mut resamp) = Fft::<f32>::new(
                        sample as usize,
                        sample_rate,
                        frame_size,
                        8,
                        1,
                        FixedSync::Output,
                    ) {
                        let mut encoder =
                            Encoder::new(sample_rate as u32, channels, Application::Audio).unwrap();
                        let mut extra = Vec::new();
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
                                let mut v = Vec::new();
                                let mut compressed = [0u8; 1024];
                                let mut buffer = [0f32; 1024];
                                while extra.len() >= frame_size {
                                    let input =
                                        InterleavedSlice::new(&extra[..frame_size], 1, frame_size)
                                            .unwrap();
                                    let mut output =
                                        InterleavedSlice::new_mut(&mut buffer, 1, frame_size)
                                            .unwrap();
                                    resamp
                                        .process_into_buffer(&input, &mut output, None)
                                        .unwrap();
                                    if let Ok(len) = encoder.encode_float(&buffer, &mut compressed)
                                        && len != 0
                                    {
                                        v.push(compressed[..len].to_vec())
                                    }
                                    extra.drain(..frame_size);
                                }
                                for v in v {
                                    let _ = tx.send(v);
                                }
                            },
                            |err| error!("Stream error: {}", err),
                            Some(Duration::from_millis(10)),
                        ) {
                            Ok(stream) => {
                                if let Ok(_s) = stream.play() {
                                    loop {
                                        if kill2.load(Ordering::Relaxed) {
                                            return;
                                        }
                                        thread::sleep(Duration::from_millis(10))
                                    }
                                } else {
                                    error!("failed to play stream")
                                }
                            }
                            Err(s) => {
                                error!(
                                    "no stream {}, {}, {}, {}",
                                    s,
                                    channel,
                                    cfg.sample_rate(),
                                    cfg.sample_format()
                                )
                            }
                        }
                    } else {
                        warn!("resamp not found")
                    }
                } else {
                    warn!("input config not found")
                }
            } else {
                warn!("input device not found")
            }
        });
        Self {
            rx,
            decoder,
            kill,
            frame_size,
        }
    }
    pub fn recv_audio<F>(&mut self, mut f: F)
    where
        F: FnMut(&[f32]),
    {
        let out = &mut [0f32; self.frame_size];
        while let Ok(data) = self.rx.try_recv() {
            if let Ok(len) = self.decoder.decode_float(&data, out, false)
                && len != 0
            {
                f(&out[..len])
            }
        }
    }
}
