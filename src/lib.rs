use audioadapter_buffers::direct::InterleavedSlice;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, StreamConfig};
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
    pub fn lock(&self) -> std::sync::MutexGuard<'_, AudioManager> {
        self.0.lock().unwrap()
    }
    pub fn try_recv_audio<F>(&self, f: F)
    where
        F: FnMut(Vec<u8>),
    {
        self.lock().try_recv_audio(f)
    }
    pub fn recv_audio<F>(&self, f: F)
    where
        F: FnMut(Vec<u8>),
    {
        self.lock().recv_audio(f)
    }
    pub fn try_recv_audio_decode<F>(&self, f: F)
    where
        F: FnMut(&mut [f32]),
    {
        self.lock().try_recv_audio_decode(f)
    }
    pub fn recv_audio_decode<F>(&self, f: F)
    where
        F: FnMut(&mut [f32]),
    {
        self.lock().recv_audio_decode(f)
    }
    pub fn decode<F>(&self, data: Vec<u8>, f: F)
    where
        F: FnMut(&mut [f32]),
    {
        self.lock().decode(data, f)
    }
    pub fn stop(&self, b: bool) {
        self.lock().stop(b)
    }
}
pub struct AudioManager {
    rx: Receiver<Vec<u8>>,
    decoder: Decoder,
    kill: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}
impl Drop for AudioManager {
    fn drop(&mut self) {
        self.kill();
    }
}
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Resource))]
pub struct AudioSettings {
    pub input_device: Option<String>,
    pub channels: Channels, //TODO only mono implemented
    pub frame_size: FrameSize,
    pub sample_rate: SampleRate,
    pub application: Application,
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
    pub fn get_input(self, sample_rate: usize) -> usize {
        (sample_rate * self.get_number()) / (48000)
    }
}
impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input_device: None,
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
            if let Some(input) = &settings.input_device
                && let Some(d) = host
                    .input_devices()
                    .map(|mut d| {
                        d.find(|d| {
                            d.description()
                                .ok()
                                .map(|a| input == a.name())
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
        let stop: Arc<AtomicBool> = AtomicBool::new(false).into();
        let stop2 = stop.clone();
        thread::spawn(move || {
            if let Some(device) = device {
                if let Ok(cfg) = device.default_input_config() {
                    let sample = cfg.sample_rate();
                    let time = frame_size.time() as u64;
                    let input_frame_size = frame_size.get_input(sample as usize);
                    let frame_size = frame_size.size(sample_rate);
                    let config = StreamConfig {
                        channels: 1,
                        sample_rate: sample,
                        buffer_size: BufferSize::Default,
                    };
                    let mut resamp = if sample as usize != sample_rate.get_number() * 1000 {
                        match Fft::<f32>::new(
                            sample as usize,
                            sample_rate.get_number() * 1000,
                            input_frame_size,
                            8,
                            1,
                            FixedSync::Both,
                        ) {
                            Ok(ret) => Some(ret),
                            Err(e) => {
                                error!("{e}");
                                return;
                            }
                        }
                    } else {
                        None
                    };
                    let mut encoder = Encoder::new(
                        (sample_rate.get_number() * 1000) as u32,
                        channels,
                        application,
                    )
                    .unwrap();
                    let mut extra = Vec::with_capacity(input_frame_size);
                    let mut compressed = [0u8; 2048];
                    let mut buffer = [0f32; 2880];
                    match device.build_input_stream(
                        &config,
                        move |data: &[f32], _| {
                            if stop2.load(Ordering::Relaxed) {
                                extra.clear();
                                return;
                            }
                            extra.extend(data);
                            while extra.len() >= input_frame_size {
                                let buf = if let Some(resamp) = &mut resamp {
                                    let input = InterleavedSlice::new(
                                        &extra[..input_frame_size],
                                        1,
                                        input_frame_size,
                                    )
                                    .unwrap();
                                    let mut output = InterleavedSlice::new_mut(
                                        &mut buffer[..frame_size],
                                        1,
                                        frame_size,
                                    )
                                    .unwrap();
                                    resamp
                                        .process_into_buffer(&input, &mut output, None)
                                        .unwrap();
                                    &buffer[..frame_size]
                                } else {
                                    &extra[..input_frame_size]
                                };
                                if let Ok(len) = encoder.encode_float(buf, &mut compressed)
                                    && len != 0
                                {
                                    let _ = tx.send(compressed[..len].to_vec());
                                }
                                extra.drain(..input_frame_size);
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
                                cfg.channels(),
                                cfg.sample_rate(),
                                cfg.sample_format()
                            )
                        }
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
        Self {
            rx,
            decoder,
            kill,
            stop,
        }
    }
    pub fn try_recv_audio<F>(&self, mut f: F)
    where
        F: FnMut(Vec<u8>),
    {
        while let Ok(data) = self.rx.try_recv() {
            f(data);
        }
    }
    pub fn recv_audio<F>(&self, mut f: F)
    where
        F: FnMut(Vec<u8>),
    {
        while let Ok(data) = self.rx.recv() {
            f(data);
        }
    }
    pub fn stop(&self, b: bool) {
        self.stop.store(b, Ordering::Relaxed)
    }
    pub fn try_recv_audio_decode<F>(&mut self, mut f: F)
    where
        F: FnMut(&mut [f32]),
    {
        let out = &mut [0.0; 2048];
        while let Ok(data) = self.rx.try_recv() {
            if let Ok(len) = self.decoder.decode_float(&data, out, false)
                && len != 0
            {
                f(&mut out[..len])
            }
        }
    }
    pub fn recv_audio_decode<F>(&mut self, mut f: F)
    where
        F: FnMut(&mut [f32]),
    {
        let out = &mut [0.0; 2048];
        while let Ok(data) = self.rx.recv() {
            if let Ok(len) = self.decoder.decode_float(&data, out, false)
                && len != 0
            {
                f(&mut out[..len])
            }
        }
    }
    pub fn decode<F>(&mut self, data: Vec<u8>, mut f: F)
    where
        F: FnMut(&mut [f32]),
    {
        let out = &mut [0.0; 2048];
        if let Ok(len) = self.decoder.decode_float(&data, out, false)
            && len != 0
        {
            f(&mut out[..len])
        }
    }
}
