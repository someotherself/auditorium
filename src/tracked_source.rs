use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

use maudio::{
    audio::{sample_rate::SampleRate, wave_shape::WaveFormType},
    data_source::{
        DataSource,
        data_source_builder::DataSourceBuilder,
        sources::{
            decoder::{
                DecoderOps, Fs,
                custom_decoder::{CustomDecoder, CustomDecoderBuilder},
            },
            noise::{Noise, NoiseBuilder, NoiseType},
            pulsewave::{PulseWave, PulseWaveBuilder},
            waveform::{WaveForm, WaveFormBuilder},
        },
    },
};

use crate::{AuditoriumError, HostResult, sources::custom_decoder::SymphoniaBackend};

pub(crate) struct PlaybackActivity(pub(crate) AtomicU32);

impl PlaybackActivity {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(PlaybackActivity(AtomicU32::new(0)))
    }
}

pub struct TrackedSource<S> {
    active_players: Arc<PlaybackActivity>,
    is_active: AtomicBool,
    pub(crate) src_length: Option<u64>,
    pub(crate) source: S,
}

impl<S> TrackedSource<S> {
    pub(crate) fn set_active(&self, yes: bool) {
        let old = self.is_active.swap(yes, Ordering::Relaxed);
        if old != yes {
            if yes {
                self.active_players.0.fetch_add(1, Ordering::Relaxed);
            } else {
                self.active_players.0.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

impl<S> TrackedSource<S> {
    pub(crate) fn inner_source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    pub(crate) fn new_decoder(
        path: &Path,
        channels: u32,
        sample_rate: SampleRate,
        tracker: Arc<PlaybackActivity>,
    ) -> HostResult<DataSource<f32, TrackedSource<CustomDecoder<f32, Fs>>>> {
        let decoder = CustomDecoderBuilder::new_f32()
            .backend::<SymphoniaBackend>()
            .channels(channels)
            .sample_rate(sample_rate)
            .from_file(path)?;
        let len = decoder.length_pcm()?;
        let src = TrackedSource {
            active_players: tracker,
            is_active: AtomicBool::new(false),
            src_length: Some(len),
            source: decoder,
        };
        let ds = DataSourceBuilder::new(channels, sample_rate)
            .build_f32::<TrackedSource<CustomDecoder<f32, Fs>>>(src)
            .map_err(AuditoriumError::from)?;
        Ok(ds)
    }

    pub(crate) fn new_wave(
        channels: u32,
        sample_rate: SampleRate,
        wave_type: WaveFormType,
        amplitude: f64,
        frequency: f64,
        tracker: Arc<PlaybackActivity>,
    ) -> HostResult<DataSource<f32, TrackedSource<WaveForm<f32>>>> {
        let wave = WaveFormBuilder::new(channels, sample_rate, wave_type, amplitude, frequency)
            .build_f32()?;
        let src = TrackedSource {
            active_players: tracker,
            is_active: AtomicBool::new(false),
            src_length: None,
            source: wave,
        };
        DataSourceBuilder::new(channels, sample_rate)
            .build_f32::<TrackedSource<WaveForm<f32>>>(src)
            .map_err(AuditoriumError::from)
    }

    pub(crate) fn new_pulse(
        channels: u32,
        sample_rate: SampleRate,
        amplitude: f64,
        frequency: f64,
        duty_cycle: f64,
        tracker: Arc<PlaybackActivity>,
    ) -> HostResult<DataSource<f32, TrackedSource<PulseWave<f32>>>> {
        let pulse = PulseWaveBuilder::new(channels, sample_rate, amplitude, frequency, duty_cycle)
            .build_f32()?;
        let src = TrackedSource {
            active_players: tracker,
            is_active: AtomicBool::new(false),
            src_length: None,
            source: pulse,
        };
        DataSourceBuilder::new(channels, sample_rate)
            .build_f32::<TrackedSource<PulseWave<f32>>>(src)
            .map_err(AuditoriumError::from)
    }

    pub(crate) fn new_noise(
        channels: u32,
        sample_rate: SampleRate,
        amplitude: f64,
        noise_type: NoiseType,
        tracker: Arc<PlaybackActivity>,
    ) -> HostResult<DataSource<f32, TrackedSource<Noise<f32>>>> {
        let noise = NoiseBuilder::new(channels, noise_type, amplitude).build_f32()?;
        let src = TrackedSource {
            active_players: tracker,
            is_active: AtomicBool::new(false),
            src_length: None,
            source: noise,
        };
        DataSourceBuilder::new(channels, sample_rate)
            .build_f32::<TrackedSource<Noise<f32>>>(src)
            .map_err(AuditoriumError::from)
    }
}
